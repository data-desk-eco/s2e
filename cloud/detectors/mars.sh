# MARS-S2L sentinel-2 methane ML (UNEP IMEO model) — python payload rsynced
# from the local repo ($MARS_DIR), cdse-native, sharded by --shard/--nshards.
mars_repo(){ echo mars-s2l; }

mars_prep(){
  local i=$1 ip; ip=$(mip "$i")
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" --delete \
    --exclude .venv --exclude .git --exclude 'out*' --exclude vendor --exclude data/cf \
    --exclude '*.nc4' --exclude '*.log' --exclude weights "$MARS_DIR/" "eouser@$ip:mars-s2l/"
  mssh "$i" 'export PATH=$HOME/.local/bin:$PATH
    cd mars-s2l && uv sync -q && uv run python - >/dev/null <<PY
from marss2l.mars_sentinel2 import plume_detection_model, s2lutils
plume_detection_model.load_model(model_name="MARS-S2L", weights_folder="weights")
s2lutils.load_model_cloud_detection("S2A")
PY'
}

mars_cmd(){ cat <<'EOS'
. /etc/profile.d/eodata.sh
export PATH=$HOME/.local/bin:$PATH
cd $HOME/mars-s2l && uv run python -m src.monitor --sites "${SITES:-$HOME/_sites.csv}" \
  --start "$START" --end "$END" --out out --shard "${SHARD:-0}" --nshards "${NSHARDS:-1}"
EOS
}

mars_merge(){ results_merge mars-s2l mars-s2l "location_name, tile"; }

mars_count(){
  echo "  mars: $(mssh "$1" 'tail -qn+2 mars-s2l/out/results_*.csv 2>/dev/null | wc -l; ls mars-s2l/out/plumes/*.png 2>/dev/null | wc -l' | tr '\n' ' ' | awk '{print $1" scenes, "$2" plumes"}')"
}

mars_pull(){
  mkdir -p "$MARS_DIR/out/cf"
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$(mip "$1"):mars-s2l/out/" "$MARS_DIR/out/cf/out-$1/"
}
