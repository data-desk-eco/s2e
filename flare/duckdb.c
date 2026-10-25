// the flare detector as a duckdb function:
//   s2_flares(b12 VARCHAR, xmin DOUBLE, ymin DOUBLE, xmax DOUBLE, ymax DOUBLE
//             [, thresholds VARCHAR])
//   → STRUCT(clear DOUBLE, error VARCHAR, flares STRUCT(x, y, max_b12,
//            avg_b12, peak_b11, ratio, peakedness, pixels, radiance,
//            saturated)[])
// thresholds is 'name=value,...' over the defaults. each chunk's rows run
// at once on flare's worker pool, so a query keeps FLARE_THREADS reads in
// flight whatever duckdb's own thread count. a failed row carries its error
// rather than failing the query.
#define DUCKDB_EXTENSION_NAME flare
#define DUCKDB_EXTENSION_API_VERSION_MAJOR 1
#define DUCKDB_EXTENSION_API_VERSION_MINOR 2
#define DUCKDB_EXTENSION_API_VERSION_PATCH 0
#include "duckdb_extension.h"
DUCKDB_EXTENSION_EXTERN

#define FLARE_LIBRARY
#include "flare.c"

typedef struct {
  task t;
  char *url;
  double box[4], th[NT];
  res r;
  const char *e;
  int *left;
  pthread_mutex_t *m;
  pthread_cond_t *c;
} row;

static void one(task *tk, ctx *x) {
  row *w = (row *)tk;
  w->e = w->e ? w->e : run(x, w->url, w->box, w->th, &w->r);
  pthread_mutex_lock(w->m);
  if (!--*w->left) pthread_cond_signal(w->c);
  pthread_mutex_unlock(w->m);
}

static int valid(duckdb_vector v, idx_t i) {
  uint64_t *m = duckdb_vector_get_validity(v);
  return !m || duckdb_validity_row_is_valid(m, i);
}

static char *str(duckdb_vector v, idx_t i) {
  duckdb_string_t *s = (duckdb_string_t *)duckdb_vector_get_data(v) + i;
  return strndup(duckdb_string_t_data(s), duckdb_string_t_length(*s));
}

static void s2_flares(duckdb_function_info info, duckdb_data_chunk in,
                      duckdb_vector out) {
  idx_t n = duckdb_data_chunk_get_size(in), cols =
    duckdb_data_chunk_get_column_count(in), total = 0;
  duckdb_vector v[6];
  row *rs = calloc(n ? n : 1, sizeof *rs);
  int left = 0;
  pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
  pthread_cond_t c = PTHREAD_COND_INITIALIZER;
  (void)info;
  start();
  for (idx_t k = 0; k < cols; k++) v[k] = duckdb_data_chunk_get_vector(in, k);
  for (idx_t i = 0; i < n; i++) {
    row *w = &rs[i];
    int null = 0;
    for (idx_t k = 0; k < 5; k++) null |= !valid(v[k], i);
    if (null) continue;
    w->url = str(v[0], i);
    for (int k = 0; k < 4; k++)
      w->box[k] = ((double *)duckdb_vector_get_data(v[k + 1]))[i];
    memcpy(w->th, TD, sizeof w->th);
    if (cols > 5 && valid(v[5], i)) {
      char *p = str(v[5], i);
      w->e = params(p, w->th);
      free(p);
    }
    if (!w->e && (w->box[2] <= w->box[0] || w->box[3] <= w->box[1]))
      w->e = "empty box";
    w->t.fn = one, w->left = &left, w->m = &m, w->c = &c, left++;
  }
  for (idx_t i = 0; i < n; i++)
    if (rs[i].url) submit(&rs[i].t);
  pthread_mutex_lock(&m);
  while (left) pthread_cond_wait(&c, &m);
  pthread_mutex_unlock(&m);

  // clear, error and the flares list; a null input row is a null result
  duckdb_vector vc = duckdb_struct_vector_get_child(out, 0),
                ve = duckdb_struct_vector_get_child(out, 1),
                vl = duckdb_struct_vector_get_child(out, 2);
  duckdb_vector_ensure_validity_writable(out);
  duckdb_vector_ensure_validity_writable(vc);
  duckdb_vector_ensure_validity_writable(ve);
  duckdb_vector_ensure_validity_writable(vl);
  uint64_t *mo = duckdb_vector_get_validity(out),
           *mc = duckdb_vector_get_validity(vc),
           *me = duckdb_vector_get_validity(ve),
           *ml = duckdb_vector_get_validity(vl);
  for (idx_t i = 0; i < n; i++) total += rs[i].e ? 0 : rs[i].r.nd;
  duckdb_list_vector_reserve(vl, total);
  duckdb_list_vector_set_size(vl, total);
  duckdb_vector f = duckdb_list_vector_get_child(vl), fc[10];
  for (int k = 0; k < 10; k++) fc[k] = duckdb_struct_vector_get_child(f, k);
  duckdb_list_entry *le = duckdb_vector_get_data(vl);
  double *clear = duckdb_vector_get_data(vc), *d[8];
  int (*px) = duckdb_vector_get_data(fc[7]);
  bool *sat = duckdb_vector_get_data(fc[9]);
  int dk[8] = {0, 1, 2, 3, 4, 5, 6, 8};
  for (int k = 0; k < 8; k++) d[k] = duckdb_vector_get_data(fc[dk[k]]);
  idx_t o = 0;
  for (idx_t i = 0; i < n; i++) {
    row *w = &rs[i];
    le[i] = (duckdb_list_entry){o, 0};
    if (!w->url) {
      duckdb_validity_set_row_invalid(mo, i);
      continue;
    }
    if (isnan(w->r.clear)) duckdb_validity_set_row_invalid(mc, i);
    else clear[i] = w->r.clear;
    if (w->e) duckdb_vector_assign_string_element(ve, i, w->e);
    else duckdb_validity_set_row_invalid(me, i);
    if (w->e) duckdb_validity_set_row_invalid(ml, i);
    for (int j = 0; !w->e && j < w->r.nd; j++, o++) {
      det *x = &w->r.d[j];
      double vals[8] = {x->x, x->y, x->max, x->avg, x->b11, x->ratio, x->pk,
                        x->rad};
      for (int k = 0; k < 8; k++) d[k][o] = vals[k];
      px[o] = x->px, sat[o] = x->sat;
    }
    le[i].length = o - le[i].offset;
    free(w->url), free(w->r.d);
  }
  free(rs);
}

static duckdb_logical_type type(duckdb_type t) {
  return duckdb_create_logical_type(t);
}

DUCKDB_EXTENSION_ENTRYPOINT(duckdb_connection con, duckdb_extension_info info,
                            struct duckdb_extension_access *access) {
  const char *fn[] = {"x", "y", "max_b12", "avg_b12", "peak_b11", "ratio",
                      "peakedness", "pixels", "radiance", "saturated"};
  const char *sn[] = {"clear", "error", "flares"};
  duckdb_logical_type ft[10], st[3], dbl = type(DUCKDB_TYPE_DOUBLE),
                      vc = type(DUCKDB_TYPE_VARCHAR);
  for (int k = 0; k < 10; k++)
    ft[k] = type(k == 7 ? DUCKDB_TYPE_INTEGER
                 : k == 9 ? DUCKDB_TYPE_BOOLEAN : DUCKDB_TYPE_DOUBLE);
  duckdb_logical_type fs = duckdb_create_struct_type(ft, fn, 10);
  st[0] = dbl, st[1] = vc, st[2] = duckdb_create_list_type(fs);
  duckdb_logical_type ret = duckdb_create_struct_type(st, sn, 3);
  duckdb_scalar_function_set set = duckdb_create_scalar_function_set(
    "s2_flares");
  for (int extra = 0; extra < 2; extra++) {
    duckdb_scalar_function f = duckdb_create_scalar_function();
    duckdb_scalar_function_set_name(f, "s2_flares");
    duckdb_scalar_function_add_parameter(f, vc);
    for (int k = 0; k < 4; k++) duckdb_scalar_function_add_parameter(f, dbl);
    if (extra) duckdb_scalar_function_add_parameter(f, vc);
    duckdb_scalar_function_set_return_type(f, ret);
    duckdb_scalar_function_set_special_handling(f);   // nulls are ours
    duckdb_scalar_function_set_volatile(f);   // one evaluation per row
    duckdb_scalar_function_set_function(f, s2_flares);
    duckdb_add_scalar_function_to_set(set, f);
    duckdb_destroy_scalar_function(&f);
  }
  duckdb_state ok = duckdb_register_scalar_function_set(con, set);
  duckdb_destroy_scalar_function_set(&set);
  for (int k = 0; k < 10; k++) duckdb_destroy_logical_type(&ft[k]);
  duckdb_destroy_logical_type(&fs), duckdb_destroy_logical_type(&st[2]);
  duckdb_destroy_logical_type(&ret), duckdb_destroy_logical_type(&dbl);
  duckdb_destroy_logical_type(&vc);
  if (ok == DuckDBError) access->set_error(info, "could not register");
  return ok == DuckDBSuccess;
}
