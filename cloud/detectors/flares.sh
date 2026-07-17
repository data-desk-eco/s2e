# s2-flares SWIR flare detection — the native repo already on every box
# (cloud-init clones + builds it; prep just freshens). archive = box.sh's
# ARCHIVER (detections/ + clouds/) + the derived cluster view.
flares_repo(){ echo s2-flares; }

flares_prep(){
  mssh "$1" "cd s2-flares && git pull -q && . \$HOME/.cargo/env && cargo build --release -q -p s2-flares-cli
    ln -sf \$HOME/_aoi.geojson _aoi.geojson"   # box.sh verify reads it here
}

flares_cmd(){ cat <<'EOS'
. /etc/profile.d/eodata.sh
cd $HOME/s2-flares && ./target/release/s2-flares detect --source cdse \
  --aoi "${AOI:-$HOME/_aoi.geojson}" --start "$START" --end "$END" \
  --buffer "${BUFFER:-2}" --out out
EOS
}

flares_merge(){
  echo 'export OUT=s2-flares/out'
  printf '%s\n' "$ARCHIVER"
  cat <<'EOS'
cd $HOME/s2-flares && S2_S3_ENDPOINT="s3.$REGION.cloudferro.com" S2_S3_REGION="$REGION" \
  S2_S3_ACCESS_KEY="$AK" S2_S3_SECRET_KEY="$SK" ./target/release/s2-flares cluster \
  --concurrency 16 --archive "s3://$BUCKET/detections/**/*.parquet" \
  --clouds "s3://$BUCKET/clouds/**/*.parquet" --out "s3://$BUCKET/clusters" \
  --start 2015-01-01 --end 2100-01-01
EOS
}

flares_count(){ echo "  flares: $(mssh "$1" 'find s2-flares/out -name "*.csv" 2>/dev/null | wc -l' | tr -d ' ') scene csvs"; }

flares_pull(){
  mkdir -p "$LOCAL_DATA"
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$(mip "$1"):s2-flares/out/" "$LOCAL_DATA/"
}
