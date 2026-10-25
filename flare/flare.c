// flare: sentinel-2 l1c swir flare detection, straight off the jp2 archive.
//
// one job is a b12 url and a box (xmin ymin xmax ymax, metres of the tile's
// own UTM zone); b11, b8a and the MSK_CLASSI cloud mask are found beside b12
// by the SAFE naming convention. urls are s3:// (signed with
// AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY against AWS_S3_ENDPOINT), https://
// or file://. this file is the whole detector and its command-line front end;
// duckdb.c wraps the same code as the s2_flares() function.
//
// why it is fast:
// - sentinel-2 jp2s carry no tile index (TLM) before baseline 05.12, so a
//   reader walks every earlier tile-part header to reach tile k, one range
//   request each. 0xff90 never occurs inside entropy-coded data, so a guess
//   at tile k's offset plus a scan for its SOT marker finds it instead, and a
//   learned per-MGRS layout makes the guess right first time. a box that
//   needs much of a band takes the whole codestream in one request.
// - a tile-part behind the file's own main header is a complete codestream,
//   and openjpeg decodes just the box from it.
// - the 60 m cloud mask (a few KB) is read first: jp2 tiles under cloud are
//   never fetched. b11 and b8a are read only around b12's candidate flares.
// - headers, masks and tile-parts are cached across jobs, so many sites on
//   one scene, or the same query with new thresholds, cost no new requests.
//
// the method is s2e's compact-source detector, run over the box in one pass:
// a background median per 320 px cell, the spectral mask, its 4-connected
// components, the morphological gates and the hot core grown from each peak.
#define _GNU_SOURCE
#include <curl/curl.h>
#include <math.h>
#include <openjpeg.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

// the burnoff compact-source gates of s2e's Thresholds::default()
static const char *TK[] = {"b12_min", "b11_min", "peak_b12_min",
  "contrast_ratio", "background_floor", "peakedness_min", "saturation",
  "hot_floor", "max_pixels", "large_pixels", "large_b12_min",
  "warm_fraction", "warm_max_pixels", "single_pixel_min", "max_cloud_local"};
static const double TD[] = {.30, .20, .50, 3, .15, 1.15, 1, .50, 80, 30, .70,
  .5, 100, .65, .75};
enum { B12MIN, B11MIN, PEAKMIN, CONTRAST, BGFLOOR, PEAKED, SAT, HOT, MAXPX,
  LARGEPX, LARGEMIN, WARMF, WARMMAX, SINGLE, MAXCLOUD, NT };
#define CELL 320   // background cell, a quarter of a 640 px jp2 tile

// "name=value,name=value" over t
static const char *params(const char *s, double *t) {
  while (s && *s) {
    const char *eq = strchr(s, '='), *end = strchr(s, ',');
    int k = 0;
    if (!eq || (end && eq > end)) return "expected name=value";
    while (k < NT && (strlen(TK[k]) != (size_t)(eq - s) ||
                      strncmp(TK[k], s, eq - s)))
      k++;
    if (k == NT) return "unknown threshold";
    t[k] = atof(eq + 1);
    s = end ? end + 1 : NULL;
  }
  return NULL;
}

// N0400+ radiometry: every band carries RADIO_ADD_OFFSET -1000 over a
// QUANTIFICATION_VALUE of 10000. older baselines have no raster cloud mask.
static double cal(int dn) { return dn ? (dn - 1000) / 10000. : 0; }

typedef struct { unsigned char *p; size_t n, cap; long long off, size; } buf;

static size_t wr(char *d, size_t s, size_t n, void *u) {
  buf *b = u;
  if (b->n + s * n > b->cap)
    b->p = realloc(b->p, b->cap = 2 * (b->n + s * n));
  memcpy(b->p + b->n, d, s * n);
  return b->n += s * n, s * n;
}

static size_t hd(char *d, size_t s, size_t n, void *u) {
  buf *b = u;
  char *slash = memchr(d, '/', s * n);
  if (!strncasecmp(d, "content-range:", 14) && slash)
    b->size = atoll(slash + 1);
  return s * n;
}

// one range [lo, hi) or, with hi < 0, the whole object; transient failures
// (network errors, 429, 5xx) back off and retry, as cdse sheds load with 503s
static const char *get(CURL *c, const char *url, long long lo, long long hi,
                       buf *b) {
  char range[48], u[4096];
  const char *s3 = getenv("AWS_S3_ENDPOINT");
  if (!strncmp(url, "s3://", 5))
    snprintf(u, sizeof u, "https://%s/%s",
             s3 ? s3 : "eodata.dataspace.copernicus.eu", url + 5);
  else
    snprintf(u, sizeof u, "%s", url);
  snprintf(range, sizeof range, "%lld-%lld", lo, hi - 1);
  curl_easy_setopt(c, CURLOPT_URL, u);
  curl_easy_setopt(c, CURLOPT_RANGE, hi < 0 ? NULL : range);
  curl_easy_setopt(c, CURLOPT_WRITEDATA, b);
  curl_easy_setopt(c, CURLOPT_HEADERDATA, b);
  for (int i = 0; i < 8; i++) {
    long code = 0;
    b->n = 0, b->size = -1;
    CURLcode e = curl_easy_perform(c);
    curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &code);
    if (!e && code < 400) {
      b->off = code == 200 ? 0 : lo;   // a server that ignored the range
      if (b->size < 0 && (hi < 0 || code == 200)) b->size = b->n;
      return NULL;
    }
    if (!e && code != 429 && code < 500) return "http error";
    if (e == CURLE_FILE_COULDNT_READ_FILE || e == CURLE_URL_MALFORMAT ||
        e == CURLE_UNSUPPORTED_PROTOCOL || e == CURLE_LOGIN_DENIED)
      return curl_easy_strerror(e);
    sleep(1u << (i < 5 ? i : 5));
  }
  return "fetch failed after retries";
}

// a direct-mapped cache of fetched bytes (headers, masks, tile-parts),
// FLARE_CACHE slots, shared by every worker
typedef struct { char *key; unsigned char *p; size_t n; long long aux; } slot;
static slot *cache;
static size_t nslot;
static pthread_mutex_t ck = PTHREAD_MUTEX_INITIALIZER;

static slot *slotof(const char *key) {
  unsigned long h = 14695981039346656037ul;
  while (*key) h = (h ^ (unsigned char)*key++) * 1099511628211ul;
  return &cache[h % nslot];
}

static int recall(const char *key, buf *out, long long *aux) {
  int hit = 0;
  if (!nslot) return 0;
  pthread_mutex_lock(&ck);
  slot *s = slotof(key);
  if ((hit = s->key && !strcmp(s->key, key))) {
    out->n = 0, wr((char *)s->p, 1, s->n, out);
    if (aux) *aux = s->aux;
  }
  pthread_mutex_unlock(&ck);
  return hit;
}

static void keep(const char *key, const void *p, size_t n, long long aux) {
  if (!nslot) return;
  unsigned char *c = malloc(n);
  char *k = strdup(key);
  memcpy(c, p, n);
  pthread_mutex_lock(&ck);
  slot *s = slotof(key);
  free(s->key), free(s->p);
  *s = (slot){k, c, n, aux};
  pthread_mutex_unlock(&ck);
}

static unsigned be(const unsigned char *p, int n) {
  unsigned v = 0;
  while (n--) v = v << 8 | *p++;
  return v;
}

// one band file: main header, geometry, and its tile-parts as read
typedef struct {
  buf h;                       // main header bytes, SOC to first SOT
  long long first, size;       // first SOT offset, file size
  int w, h_, tw, th, nx, nt;   // image, tile, tiles across, tile count
  double ox, oy, res;          // upper-left corner and pixel size, metres
  long long at[256], len[256]; // tile-parts located, 0 = unknown
  buf tp[256];                 // tile-part bytes
  char have[256];
} band;

typedef struct { CURL *c; buf b, s; band bd[3]; unsigned short *px; } ctx;

static const char *header(ctx *x, const char *url, band *f) {
  char key[4200];
  buf *b = &x->b;
  long long size = -1;
  snprintf(key, sizeof key, "%s#h", url);
  if (!recall(key, b, &size)) {
    const char *e = get(x->c, url, 0, 16384, b);
    if (e) return e;
    if ((size = b->size) < 0) {   // no Content-Range (file://)
      curl_off_t n = -1;
      curl_easy_setopt(x->c, CURLOPT_NOBODY, 1L);
      curl_easy_perform(x->c);
      curl_easy_getinfo(x->c, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &n);
      curl_easy_setopt(x->c, CURLOPT_NOBODY, 0L);
      curl_easy_setopt(x->c, CURLOPT_HTTPGET, 1L);
      if ((size = n) < 0) return "unknown file size";
    }
    keep(key, b->p, b->n, size);
  }
  unsigned char *s = memmem(b->p, b->n, "\xff\x4f\xff\x51", 4);
  unsigned char *sot = s ? memmem(s, b->p + b->n - s, "\xff\x90", 2) : 0;
  if (!sot || be(s + 4, 2) < 41 || be(s + 40, 2) != 1 ||
      be(s + 16, 4) | be(s + 20, 4) | be(s + 32, 4) | be(s + 36, 4))
    return "no single-component, origin-anchored codestream header";
  // the GMLJP2 box ahead of the codestream: pixel (1,1)'s centre, then the
  // pixel size as the first offset vector
  char *g = memmem(b->p, s - b->p, "<gml:pos>", 9);
  char *o = g ? memmem(g, s - (unsigned char *)g, "<gml:offsetVector", 17) : 0;
  if (!o || !(o = memchr(o, '>', 128)) ||
      sscanf(g + 9, "%lf %lf", &f->ox, &f->oy) != 2 ||
      sscanf(o + 1, "%lf", &f->res) != 1 || f->res <= 0)
    return "no GMLJP2 georeferencing";
  f->ox -= f->res / 2, f->oy += f->res / 2;
  f->w = be(s + 8, 4), f->h_ = be(s + 12, 4);
  f->tw = be(s + 24, 4), f->th = be(s + 28, 4);
  f->nx = (f->w + f->tw - 1) / f->tw;
  f->nt = f->nx * ((f->h_ + f->th - 1) / f->th);
  if (f->nt > 256 || f->tw % CELL || f->th % CELL)
    return "unexpected tiling";
  f->h.n = 0, wr((char *)s, 1, sot - s, &f->h);
  f->first = sot - b->p, f->size = size;
  memset(f->at, 0, sizeof f->at), memset(f->have, 0, sizeof f->have);
  return NULL;
}

// a tile-part header: SOT, Lsot = 10, a tile index in range, the only part
static int sot(const unsigned char *p, int nt, int *k, long long *len) {
  if (p[0] != 0xff || p[1] != 0x90 || be(p + 2, 2) != 10) return 0;
  *k = be(p + 4, 2), *len = be(p + 6, 4);
  return *k < nt && !p[10] && p[11] <= 1 && *len >= 14;
}

// tile-part k: fetch around a predicted offset (the expected length, when
// known, sizing the read), scan for SOT markers and re-aim from the nearest
// one found until k is in hand
static const char *tile(ctx *x, const char *url, band *f, int k,
                        long long pred, long long want) {
  long long M = want ? 64 << 10 : 256 << 10, lo = pred - M;
  long long W = 2 * M + (want ? want + want / 8 : 512 << 10);
  double avg = (double)(f->size - f->first) / f->nt;
  buf *b = &x->b, *out = &f->tp[k];
  for (int i = 0; i < 12; i++) {
    lo = lo + W > f->size ? f->size - W : lo;
    lo = lo < f->first ? f->first : lo;
    const char *e = get(x->c, url, lo, lo + W, b);
    if (e) return e;
    long long below = -1, above = -1, len;
    int kb = -1, ka = -1, j;
    for (size_t q = 0; q + 12 <= b->n; q++) {
      if (b->p[q] != 0xff || !sot(b->p + q, f->nt, &j, &len)) continue;
      long long at = b->off + q;
      if (at + len > f->size) continue;
      f->at[j] = at, f->len[j] = len;
      if (j == k) {
        out->n = 0;
        if (q + len <= b->n) return wr((char *)b->p + q, 1, len, out), NULL;
        if ((e = get(x->c, url, at, at + len, out))) return e;
        return out->n == (size_t)len ? NULL : "short tile-part";
      }
      if (j < k && j > kb) kb = j, below = at + len;
      if (j > k && (ka < 0 || j < ka)) ka = j, above = at;
    }
    M = 256 << 10, W = 2 * M + (512 << 10);
    lo = kb == k - 1 ? below + M
       : kb >= 0     ? below + (long long)((k - kb - 1) * avg)
       : ka >= 0     ? above - (long long)((ka - k) * avg)
       : lo + W + M;
    lo -= M;
  }
  return "tile-part not found";
}

// learned layouts: where each tile-part sat, as fractions of file size, per
// MGRS tile and band. the land/sea/cloud mix that sizes a tile changes little
// between dates, so after one file the next is read in one request per tile
typedef struct { char key[16]; float at[256], len[256]; } layout;
static layout lay[4096];
static int nlay;
static pthread_mutex_t lk = PTHREAD_MUTEX_INITIALIZER;

// T13SFR_20250519T172859_B11.jp2 → T13SFR_B11.jp2, or with band, _B12.jp2
static layout *learned(const char *url, const char *band) {
  const char *f = strrchr(url, '/') ? strrchr(url, '/') + 1 : url;
  const char *b = strrchr(f, '_');
  char key[16];
  snprintf(key, sizeof key, "%.6s%.10s", f, band ? band : b ? b : "");
  for (int i = 0; i < nlay; i++)
    if (!strcmp(lay[i].key, key)) return &lay[i];
  if (nlay == 4096) return NULL;
  memset(&lay[nlay], 0, sizeof *lay);
  return strcpy(lay[nlay].key, key), &lay[nlay++];
}

// every tile-part in need[]: from the cache, else the whole codestream in
// one request when much of the band is wanted, else one at a time
static const char *parts(ctx *x, const char *url, band *f, const char *need) {
  char key[4200];
  int miss = 0;
  for (int k = 0; k < f->nt; k++) {
    snprintf(key, sizeof key, "%s#%d", url, k);
    if (need[k] && !f->have[k] && !(f->have[k] = recall(key, &f->tp[k], 0)))
      miss++;
  }
  if (!miss) return NULL;
  if (miss > f->nt / 4) {
    const char *e = get(x->c, url, f->first, f->size, &x->b);
    long long len;
    int k;
    if (e) return e;
    for (size_t q = 0; q + 12 <= x->b.n && sot(x->b.p + q, f->nt, &k, &len) &&
                       q + len <= x->b.n; q += len) {
      f->tp[k].n = 0, wr((char *)x->b.p + q, 1, len, &f->tp[k]);
      f->at[k] = f->first + q, f->len[k] = len, f->have[k] = 1;
      snprintf(key, sizeof key, "%s#%d", url, k);
      keep(key, x->b.p + q, len, 0);
    }
  }
  // a band not yet seen on this MGRS tile borrows b12's layout, which the
  // same date's b12 read has just taught
  layout l = {0}, *g;
  pthread_mutex_lock(&lk);
  if ((g = learned(url, NULL))) l = *g;
  for (int k = 0; (g = learned(url, "_B12.jp2")) && k < 256; k++)
    if (!l.at[k]) l.at[k] = g->at[k], l.len[k] = g->len[k];
  pthread_mutex_unlock(&lk);
  for (int k = 0; k < f->nt; k++) {
    if (!need[k] || f->have[k]) continue;
    long long p = l.at[k]
      ? (long long)(l.at[k] * f->size)
      : f->first + (long long)((double)k * (f->size - f->first) / f->nt);
    const char *e = tile(x, url, f, k, p, (long long)(l.len[k] * f->size));
    if (e) return e;
    f->have[k] = 1;
    snprintf(key, sizeof key, "%s#%d", url, k);
    keep(key, f->tp[k].p, f->tp[k].n, 0);
  }
  pthread_mutex_lock(&lk);
  if ((g = learned(url, NULL)))
    for (int k = 0; k < f->nt; k++)
      if (f->at[k])
        g->at[k] = (double)f->at[k] / f->size,
        g->len[k] = (double)f->len[k] / f->size;
  pthread_mutex_unlock(&lk);
  return NULL;
}

typedef struct { const unsigned char *p; size_t n, i; } mem;
static OPJ_SIZE_T mr(void *d, OPJ_SIZE_T n, void *u) {
  mem *m = u;
  if (m->i >= m->n) return (OPJ_SIZE_T)-1;
  n = n < m->n - m->i ? n : m->n - m->i;
  memcpy(d, m->p + m->i, n);
  return m->i += n, n;
}
static OPJ_OFF_T ms(OPJ_OFF_T n, void *u) {
  mem *m = u;
  n = n < (OPJ_OFF_T)(m->n - m->i) ? n : (OPJ_OFF_T)(m->n - m->i);
  return m->i += n, n;
}
static OPJ_BOOL mk(OPJ_OFF_T n, void *u) {
  mem *m = u;
  return n <= (OPJ_OFF_T)m->n ? (m->i = n, 1) : 0;
}

// decode window [x0,x1)×[y0,y1) of every component into out, comp-major
static const char *decode(const buf *s, OPJ_CODEC_FORMAT fmt, int x0, int y0,
                          int x1, int y1, int nc, unsigned short *out) {
  opj_codec_t *c = opj_create_decompress(fmt);
  opj_stream_t *st = opj_stream_default_create(1);
  opj_dparameters_t d;
  opj_image_t *im = NULL;
  mem m = {s->p, s->n, 0};
  opj_set_default_decoder_parameters(&d);
  opj_stream_set_user_data(st, &m, NULL);
  opj_stream_set_user_data_length(st, s->n);
  opj_stream_set_read_function(st, mr);
  opj_stream_set_skip_function(st, ms);
  opj_stream_set_seek_function(st, mk);
  int ok = opj_setup_decoder(c, &d) && opj_read_header(st, c, &im) &&
           (int)im->numcomps >= nc &&
           opj_set_decode_area(c, im, x0, y0, x1, y1) &&
           opj_decode(c, st, im) && opj_end_decompress(c, st);
  for (int k = 0, n = (x1 - x0) * (y1 - y0); ok && k < nc; k++)
    if (im->comps[k].w * im->comps[k].h != (unsigned)n) ok = 0;
    else for (int i = 0; i < n; i++) out[k * n + i] = im->comps[k].data[i];
  opj_image_destroy(im), opj_stream_destroy(st), opj_destroy_codec(c);
  return ok ? NULL : "jp2 decode failed";
}

// tile k's pixels under image window [x0,x1)×[y0,y1), clipped to the tile,
// into out (row stride ow, origin ox0,oy0)
static const char *dec(ctx *x, band *f, int k, int x0, int y0, int x1, int y1,
                       unsigned short *out, int ow, int ox0, int oy0) {
  int tx = k % f->nx * f->tw, ty = k / f->nx * f->th;
  x0 = x0 > tx ? x0 : tx, y0 = y0 > ty ? y0 : ty;
  x1 = x1 < tx + f->tw ? x1 : tx + f->tw;
  y1 = y1 < ty + f->th ? y1 : ty + f->th;
  if (x1 <= x0 || y1 <= y0) return NULL;
  x->s.n = 0, wr((char *)f->h.p, 1, f->h.n, &x->s);
  wr((char *)f->tp[k].p, 1, f->tp[k].n, &x->s), wr("\xff\xd9", 1, 2, &x->s);
  x->px = realloc(x->px, (size_t)(x1 - x0) * (y1 - y0) * 2);
  const char *e = decode(&x->s, OPJ_CODEC_J2K, x0, y0, x1, y1, 1, x->px);
  for (int y = y0; !e && y < y1; y++)
    memcpy(out + (size_t)(y - oy0) * ow + x0 - ox0,
           x->px + (size_t)(y - y0) * (x1 - x0), (x1 - x0) * 2);
  return e;
}

static char *swap(const char *url, const char *from, const char *to) {
  const char *p = strstr(url, from);
  char *r = malloc(strlen(url) + strlen(to) + 1);
  if (!p) return free(r), NULL;
  sprintf(r, "%.*s%s", (int)(p - url), url, to);
  return r;
}

// one detection; x y is the peak pixel's centre in the box's UTM metres
typedef struct {
  double x, y, max, avg, b11, ratio, pk, rad;
  int px, sat;
} det;
typedef struct { double clear; det *d; int nd, cap; } res;

static int find(int *p, int i) {
  while (p[i] != i) i = p[i] = p[p[i]];
  return i;
}

// position of box index v among the sorted candidate indices, or -1
static int at(const int *a, int n, int v) {
  int lo = 0, hi = n;
  while (lo < hi) {
    int m = (lo + hi) / 2;
    if (a[m] < v) lo = m + 1; else hi = m;
  }
  return lo < n && a[lo] == v ? lo : -1;
}

// one job: the flares in a box of one scene, and its clear-sky share
static const char *run(ctx *x, const char *url, const double *box,
                       const double *t, res *r) {
  band *f = x->bd;
  const char *nb = strstr(url, "_N"), *e = NULL;
  r->nd = 0, r->clear = NAN;
  if (!nb || atoi(nb + 2) < 400) return "needs baseline N0400 or later";
  char *u[3] = {swap(url, "B12.jp2", "B11.jp2"),
                swap(url, "B12.jp2", "B8A.jp2"),
                swap(url, "/IMG_DATA/", "/QI_DATA/MSK_CLASSI_B00.jp2")};
  unsigned short *b12 = NULL, *m = NULL, *v11 = NULL, *v8a = NULL;
  int *cnt = NULL, *cld = NULL, *idx = NULL, *par = NULL, *pq = NULL;
  double *ct = NULL, *bgm = NULL;
  char need[256] = {0}, *ok = NULL;
  int X0, Y0, X1, Y1, W, H, mc = 0;
  if (!u[0] || !u[1] || !u[2]) { e = "b12 url not in SAFE form"; goto out; }
  if ((e = header(x, url, f))) goto out;
  {
    double v[4] = {floor((box[0] - f->ox) / f->res),
                   floor((f->oy - box[3]) / f->res),
                   ceil((box[2] - f->ox) / f->res),
                   ceil((f->oy - box[1]) / f->res)};
    int lim[4] = {f->w, f->h_, f->w, f->h_}, p[4];
    for (int i = 0; i < 4; i++)
      p[i] = v[i] < 0 ? 0 : v[i] > lim[i] ? lim[i] : v[i];
    X0 = p[0], Y0 = p[1], X1 = p[2], Y1 = p[3], W = X1 - X0, H = Y1 - Y0;
  }
  if (W <= 0 || H <= 0) goto out;   // the box misses this tile

  // the 60 m mask, whole (a few KB): opaque cloud and cirrus layers
  char key[4200];
  int cx0 = X0 / 3, cy0 = Y0 / 3, cw = (X1 + 2) / 3 - cx0;
  int ch = (Y1 + 2) / 3 - cy0;
  snprintf(key, sizeof key, "%s#c", u[2]);
  if (!recall(key, &x->s, NULL)) {
    if ((e = get(x->c, u[2], 0, -1, &x->s))) goto out;
    keep(key, x->s.p, x->s.n, 0);
  }
  m = malloc((size_t)2 * cw * ch * 2);
  if ((e = decode(&x->s, OPJ_CODEC_JP2, cx0, cy0, cx0 + cw, cy0 + ch, 2, m)))
    goto out;
#define CLOUDY(row, col)                                                    \
  (m[((Y0 + (row)) / 3 - cy0) * cw + (X0 + (col)) / 3 - cx0] ||            \
   m[cw * ch + ((Y0 + (row)) / 3 - cy0) * cw + (X0 + (col)) / 3 - cx0])

  // background cells: cloud first, so jp2 tiles under cloud are never read
  int gx0 = X0 / CELL, gy0 = Y0 / CELL, ncx = (X1 - 1) / CELL - gx0 + 1;
  int nc = ncx * ((Y1 - 1) / CELL - gy0 + 1), seen = 0, clear = 0;
  cnt = calloc(nc, sizeof *cnt), cld = calloc(nc, sizeof *cld);
  ct = malloc(nc * sizeof *ct), bgm = malloc(nc * sizeof *bgm);
#define CELLOF(row, col) \
  (((Y0 + (row)) / CELL - gy0) * ncx + (X0 + (col)) / CELL - gx0)
  for (int r0 = 0; r0 < H; r0++)
    for (int c0 = 0; c0 < W; c0++) {
      int c = CELLOF(r0, c0), cl = CLOUDY(r0, c0);
      cnt[c]++, cld[c] += cl;
      seen++, clear += !cl;
    }
  for (int c = 0; c < nc; c++)
    if (cld[c] <= t[MAXCLOUD] * cnt[c]) {
      int gx = gx0 + c % ncx, gy = gy0 + c / ncx;
      need[gy * CELL / f->th * f->nx + gx * CELL / f->tw] = 1;
    }
  b12 = calloc((size_t)W * H, 2);
  if ((e = parts(x, url, f, need))) goto out;
  for (int k = 0; k < f->nt; k++)
    if (need[k] && (e = dec(x, f, k, X0, Y0, X1, Y1, b12, W, X0, Y0)))
      goto out;

  // per cell: no data counts as obscured; the median of the dim pixels
  // (below b12_min) is the background, from a histogram of their counts
  int cut = (int)ceil(1000 + t[B12MIN] * 10000), *hist = calloc(cut, 4);
  for (int c = 0; c < nc; c++) {
    int gx = gx0 + c % ncx, gy = gy0 + c / ncx, nbg = 0, nd = 0, ndc = 0;
    int ax = gx * CELL > X0 ? gx * CELL - X0 : 0, ay = gy * CELL > Y0
             ? gy * CELL - Y0 : 0;
    int bx = (gx + 1) * CELL - X0 < W ? (gx + 1) * CELL - X0 : W;
    int by = (gy + 1) * CELL - Y0 < H ? (gy + 1) * CELL - Y0 : H;
    ct[c] = INFINITY;
    if (cld[c] > t[MAXCLOUD] * cnt[c]) continue;
    memset(hist, 0, cut * 4);
    for (int r0 = ay; r0 < by; r0++)
      for (int c0 = ax; c0 < bx; c0++) {
        int dn = b12[(size_t)r0 * W + c0];
        if (!dn) nd++, ndc += !CLOUDY(r0, c0);
        else if (dn < cut && cal(dn) < t[B12MIN]) hist[dn]++, nbg++;
      }
    seen -= nd, clear -= ndc;
    if (cld[c] + ndc > t[MAXCLOUD] * cnt[c] || nbg < 10) continue;
    int i = 0;
    for (int s = 0; (s += hist[i]) <= nbg / 2; i++);
    double med = bgm[c] = cal(i);
    ct[c] = (med > t[BGFLOOR] ? med : t[BGFLOOR]) * t[CONTRAST];
  }
  free(hist);
  if (seen > 0) r->clear = (double)clear / seen;

  // candidates: b12 alone passes (above b12_min and the cell's contrast),
  // in raster order. the spectral mask is a subset, and a detection needs
  // a peak of peak_b12_min, so a box without one is done
  int hot = 0, cap = 0;
  for (int r0 = 0; r0 < H; r0++)
    for (int c0 = 0; c0 < W; c0++) {
      double v = cal(b12[(size_t)r0 * W + c0]);
      if (v <= t[B12MIN] || v <= ct[CELLOF(r0, c0)]) continue;
      if (mc == cap) idx = realloc(idx, (cap = cap ? 2 * cap : 256) * 4);
      idx[mc++] = r0 * W + c0, hot |= v >= t[PEAKMIN];
    }
  if (!hot) goto out;
#define V(j) cal(b12[idx[j]])

  // components of the candidates; only those holding a possible peak need
  // b11 and b8a, read over their bounding boxes in each jp2 tile
  par = malloc(mc * 4), ok = calloc(mc, 1);
  for (int j = 0; j < mc; j++) {
    int i = idx[j], n;
    par[j] = j;
    if (i % W && j && idx[j - 1] == i - 1) par[find(par, j)] = find(par, j - 1);
    if (i >= W && (n = at(idx, j, i - W)) >= 0)
      par[find(par, j)] = find(par, n);
  }
  for (int j = 0; j < mc; j++)
    if (V(j) >= t[PEAKMIN]) ok[find(par, j)] = 1;
  int bb[256][4];
  memset(need, 0, sizeof need);
  for (int j = 0; j < mc; j++) {
    if (!ok[find(par, j)]) continue;
    int gx = X0 + idx[j] % W, gy = Y0 + idx[j] / W;
    int k = gy / f->th * f->nx + gx / f->tw, *b = bb[k];
    if (!need[k]) b[0] = b[2] = gx, b[1] = b[3] = gy, need[k] = 1;
    b[0] = gx < b[0] ? gx : b[0], b[1] = gy < b[1] ? gy : b[1];
    b[2] = gx > b[2] ? gx : b[2], b[3] = gy > b[3] ? gy : b[3];
  }
  v11 = malloc(mc * 2), v8a = malloc(mc * 2);
  for (int bi = 1; bi <= 2; bi++) {
    band *g = &x->bd[bi];
    unsigned short *dst = bi == 1 ? v11 : v8a;
    if ((e = header(x, u[bi - 1], g))) goto out;
    if (g->w != f->w || g->h_ != f->h_ || g->tw != f->tw || g->ox != f->ox ||
        g->oy != f->oy) { e = "bands on different grids"; goto out; }
    if ((e = parts(x, u[bi - 1], g, need))) goto out;
    for (int k = 0; k < g->nt; k++) {
      if (!need[k]) continue;
      int *b = bb[k], bw = b[2] - b[0] + 1, bh = b[3] - b[1] + 1;
      unsigned short *tmp = malloc((size_t)bw * bh * 2);
      if ((e = dec(x, g, k, b[0], b[1], b[2] + 1, b[3] + 1, tmp, bw, b[0],
                   b[1]))) {
        free(tmp);
        goto out;
      }
      for (int j = 0; j < mc; j++) {
        int gx = X0 + idx[j] % W, gy = Y0 + idx[j] / W;
        if (ok[find(par, j)] && gy / g->th * g->nx + gx / g->tw == k)
          dst[j] = tmp[(size_t)(gy - b[1]) * bw + gx - b[0]];
      }
      free(tmp);
    }
  }

  // the spectral mask on those, its components, and per component the
  // count, the sum and the lowest-index peak
  for (int j = 0; j < mc; j++) ok[j] = ok[find(par, j)];
  for (int j = 0; j < mc; j++) {
    double a = cal(v11[j]), c = cal(v8a[j]), s = a + c, v = V(j);
    double nh = s > .01 ? (a - c) / s : 0;
    ok[j] = ok[j] && a > t[B11MIN] && (nh > 0 || a > t[SAT] || v > t[SAT]);
  }
  for (int j = 0; j < mc; j++) {
    int i = idx[j], n;
    par[j] = j;
    if (!ok[j]) continue;
    if (i % W && j && idx[j - 1] == i - 1 && ok[j - 1])
      par[find(par, j)] = find(par, j - 1);
    if (i >= W && (n = at(idx, j, i - W)) >= 0 && ok[n])
      par[find(par, j)] = find(par, n);
  }
  int *n_ = calloc(mc, 4), *pk = malloc(mc * 4);
  double *sum = calloc(mc, 8);
  char *vis = calloc(mc, 1);
  pq = malloc(mc * 4);
  for (int j = 0; j < mc; j++) {
    if (!ok[j]) continue;
    int l = find(par, j);
    if (!n_[l]++ || V(j) > V(pk[l])) pk[l] = j;
    sum[l] += V(j);
  }
  for (int l = 0; l < mc; l++) {
    if (!n_[l]) continue;
    double np = n_[l], p = V(pk[l]), avg = sum[l] / np;
    if (np > t[MAXPX] || p < t[PEAKMIN]) continue;
    if (np > t[LARGEPX] && p < t[LARGEMIN]) continue;
    if (n_[l] > 1 && p < t[PEAKED] * avg && avg < t[SAT]) continue;
    if (n_[l] == 1 && p < t[SINGLE]) continue;
    // the hot core: contiguous from the peak within this component, above
    // max(peak·warm_fraction, hot_floor), capped at warm_max_pixels
    // and its integrated excess radiance, Σ(b12 − the cell's background)
    int pi = idx[pk[l]], hd = 0, tl = 1, size = 0;
    double th = p * t[WARMF] > t[HOT] ? p * t[WARMF] : t[HOT], rad = 0;
    double bg = bgm[CELLOF(pi / W, pi % W)];
    vis[pq[0] = pk[l]] = 1;
    while (hd < tl) {
      int j = pq[hd++], i = idx[j], nb4[4], k = 0;
      size++, rad += V(j) > bg ? V(j) - bg : 0;
      if (size > t[WARMMAX]) break;
      if (i >= W) nb4[k++] = at(idx, mc, i - W);
      if (i + W < W * H) nb4[k++] = at(idx, mc, i + W);
      if (i % W) nb4[k++] = j > 0 && idx[j - 1] == i - 1 ? j - 1 : -1;
      if ((i + 1) % W) nb4[k++] = j + 1 < mc && idx[j + 1] == i + 1 ? j + 1
                                                                    : -1;
      for (int z = 0; z < k; z++) {
        int q = nb4[z];
        if (q >= 0 && !vis[q] && ok[q] && find(par, q) == l && V(q) > th)
          vis[pq[tl++] = q] = 1;
      }
    }
    for (int z = 0; z < tl; z++) vis[pq[z]] = 0;
    if (size > t[WARMMAX]) continue;
    double a = cal(v11[pk[l]]);
    if (r->nd == r->cap) r->d = realloc(r->d, (r->cap = 2 * r->cap + 8) *
                                                  sizeof *r->d);
    r->d[r->nd++] = (det){f->ox + (X0 + pi % W + .5) * f->res,
                          f->oy - (Y0 + pi / W + .5) * f->res, p, avg, a,
                          a > 1e-6 ? p / a : INFINITY, p / avg, rad, size,
                          p >= t[SAT]};
  }
  free(n_), free(pk), free(sum), free(vis);
out:
  free(u[0]), free(u[1]), free(u[2]), free(b12), free(m), free(v11);
  free(v8a), free(cnt), free(cld), free(idx), free(par), free(pq), free(ct);
  free(bgm), free(ok);
  return e;
}

// a pool of workers, each with its own connection, running queued tasks
typedef struct task { void (*fn)(struct task *, ctx *); struct task *next; }
  task;
static task *qh, *qt;
static pthread_mutex_t qm = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t qc = PTHREAD_COND_INITIALIZER;

static void *worker(void *_) {
  ctx x = {0};
  const char *id = getenv("AWS_ACCESS_KEY_ID"),
             *secret = getenv("AWS_SECRET_ACCESS_KEY"),
             *region = getenv("AWS_REGION");
  char sig[64], *cred = NULL;
  (void)_;
  x.c = curl_easy_init();
  snprintf(sig, sizeof sig, "aws:amz:%s:s3", region ? region : "us-east-1");
  if (id && secret && asprintf(&cred, "%s:%s", id, secret) > 0) {
    curl_easy_setopt(x.c, CURLOPT_AWS_SIGV4, sig);
    curl_easy_setopt(x.c, CURLOPT_USERPWD, cred);
  }
  curl_easy_setopt(x.c, CURLOPT_WRITEFUNCTION, wr);
  curl_easy_setopt(x.c, CURLOPT_HEADERFUNCTION, hd);
  curl_easy_setopt(x.c, CURLOPT_CONNECTTIMEOUT, 15L);
  curl_easy_setopt(x.c, CURLOPT_LOW_SPEED_LIMIT, 1024L);   // a stalled
  curl_easy_setopt(x.c, CURLOPT_LOW_SPEED_TIME, 30L);      // read retries
  for (;;) {
    pthread_mutex_lock(&qm);
    while (!qh) pthread_cond_wait(&qc, &qm);
    task *t = qh;
    if (!(qh = t->next)) qt = NULL;
    pthread_mutex_unlock(&qm);
    t->fn(t, &x);
  }
  return NULL;
}

static void submit(task *t) {
  pthread_mutex_lock(&qm);
  t->next = NULL;
  if (qt) qt->next = t; else qh = t;
  qt = t;
  pthread_cond_signal(&qc);
  pthread_mutex_unlock(&qm);
}

// FLARE_THREADS workers (default 64), FLARE_CACHE cache slots (default
// 4096; tile-parts are ~0.4 MB, so at most ~1.6 GB)
static void init(void) {
  const char *n = getenv("FLARE_THREADS"), *c = getenv("FLARE_CACHE");
  int jobs = n ? atoi(n) : 64;
  pthread_t th;
  curl_global_init(CURL_GLOBAL_ALL);
  nslot = c ? strtoul(c, NULL, 10) : 4096;
  cache = calloc(nslot ? nslot : 1, sizeof *cache);
  for (int i = 0; i < (jobs < 1 ? 1 : jobs); i++)
    pthread_create(&th, NULL, worker, NULL), pthread_detach(th);
}

static void start(void) {
  static pthread_once_t once = PTHREAD_ONCE_INIT;
  pthread_once(&once, init);
}

#ifndef FLARE_LIBRARY
// the command line: jobs on stdin, one per line, tab-separated
//   key  b12-url  xmin  ymin  xmax  ymax
// and on stdout, tab-separated with a header, one row per detection, or one
// row with empty detection columns when the box holds none:
//   key status clear x y max_b12 avg_b12 peak_b11 ratio peakedness pixels
//   radiance saturated
// status is ok or err (the reason on stderr, and the exit status is 1).
// arguments name=value override thresholds.
typedef struct { task t; char *line; } cli;
static double T[NT];
static int pending, failed;
static pthread_mutex_t om = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t oc = PTHREAD_COND_INITIALIZER;

static void line(task *tk, ctx *x) {
  cli *j = (cli *)tk;
  char *f[6], *sv, *end, *row = NULL;
  size_t len;
  int k = 0, bad = 0;
  double box[4];
  res r = {0};
  const char *e = NULL;
  for (char *s = strtok_r(j->line, "\t\n", &sv); s && k < 6;
       s = strtok_r(NULL, "\t\n", &sv))
    f[k++] = s;
  for (int i = 0; i < 4 && k == 6; i++)
    if (box[i] = strtod(f[i + 2], &end), *end || end == f[i + 2]) bad = 1;
  if (!k) f[0] = "";
  if (k < 6 || bad) e = "expected key, b12 url, xmin, ymin, xmax, ymax";
  else if (box[2] <= box[0] || box[3] <= box[1]) e = "empty box";
  else e = run(x, f[1], box, T, &r);
  FILE *o = open_memstream(&row, &len);
  char c[32] = "";
  if (!isnan(r.clear)) snprintf(c, sizeof c, "%.6g", r.clear);
  if (e) fprintf(o, "%s\terr\t\t\t\t\t\t\t\t\t\t\t\n", f[0]);
  else if (!r.nd) fprintf(o, "%s\tok\t%s\t\t\t\t\t\t\t\t\t\t\n", f[0], c);
  for (int i = 0; !e && i < r.nd; i++) {
    det *d = &r.d[i];
    fprintf(o, "%s\tok\t%s\t%.1f\t%.1f\t%.10g\t%.10g\t%.10g\t%.10g\t%.10g"
               "\t%d\t%.10g\t%d\n", f[0], c, d->x, d->y, d->max, d->avg,
            d->b11, d->ratio, d->pk, d->px, d->rad, d->sat);
  }
  fclose(o);
  pthread_mutex_lock(&om);
  if (e) fprintf(stderr, "%s: %s\n", f[0], e), failed = 1;
  fputs(row, stdout), fflush(stdout);
  pending--, pthread_cond_signal(&oc);
  pthread_mutex_unlock(&om);
  free(row), free(r.d), free(j->line), free(j);
}

int main(int argc, char **argv) {
  memcpy(T, TD, sizeof T);
  for (int i = 1; i < argc; i++)
    if (params(argv[i], T)) {
      fprintf(stderr, "usage: flare [threshold=value ...] < jobs\n"
                      "  (FLARE_THREADS workers, FLARE_CACHE slots)\n");
      for (int k = 0; k < NT; k++) fprintf(stderr, "  %s=%g\n", TK[k], TD[k]);
      return 2;
    }
  start();
  puts("key\tstatus\tclear\tx\ty\tmax_b12\tavg_b12\tpeak_b11\tratio"
       "\tpeakedness\tpixels\tradiance\tsaturated");
  fflush(stdout);
  char *l = NULL;
  size_t cap = 0;
  while (getline(&l, &cap, stdin) > 0) {
    if (*l == '#' || *l == '\n') continue;
    cli *j = malloc(sizeof *j);
    j->t.fn = line, j->line = strdup(l);
    pthread_mutex_lock(&om);
    while (pending >= 1024) pthread_cond_wait(&oc, &om);   // backpressure
    pending++;
    pthread_mutex_unlock(&om);
    submit(&j->t);
  }
  pthread_mutex_lock(&om);
  while (pending) pthread_cond_wait(&oc, &om);
  pthread_mutex_unlock(&om);
  return failed;
}
#endif
