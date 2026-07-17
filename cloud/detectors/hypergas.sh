# hypergas EMIT hyperspectral methane (SRON matched filter → plume mask → IME)
# — python payload rsynced from the local repo ($HYPERGAS_DIR). needs an
# earthdata login: ~/.netrc with urs.earthdata.nasa.gov is copied to the box
# when present; without it the payload errors per-granule (rows say so).
hypergas_repo(){ echo hypergas; }

hypergas_prep(){
  local i=$1 ip; ip=$(mip "$i")
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" --delete \
    --exclude .venv --exclude .git --exclude 'out*' --exclude resources --exclude 'data/*.nc' \
    "$HYPERGAS_DIR/" "eouser@$ip:hypergas/"
  if grep -qs urs.earthdata.nasa.gov "$HOME/.netrc"; then
    scp -q $SSHOPTS -i "$KEYFILE" "$HOME/.netrc" "eouser@$ip:.netrc"
  else
    say "  [$i] no earthdata ~/.netrc — hypergas downloads will fail until one is provided"
  fi
  mssh "$i" 'export PATH=$HOME/.local/bin:$PATH; cd hypergas && uv sync -q && make -s resources'
}

hypergas_cmd(){ cat <<'EOS'
export PATH=$HOME/.local/bin:$PATH
cd $HOME/hypergas && uv run python bulk.py --sites "${SITES:-$HOME/_sites.csv}" \
  --start "$START" --end "$END" --out out --shard "${SHARD:-0}" --nshards "${NSHARDS:-1}"
EOS
}

hypergas_merge(){ results_merge hypergas hypergas "location_name, granule"; }

hypergas_count(){
  echo "  hypergas: $(mssh "$1" 'tail -qn+2 hypergas/out/results_*.csv 2>/dev/null | wc -l; ls hypergas/out/plumes/*.png 2>/dev/null | wc -l' | tr '\n' ' ' | awk '{print $1" granules, "$2" plumes"}')"
}

hypergas_pull(){
  mkdir -p "$HYPERGAS_DIR/out/cf"
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$(mip "$1"):hypergas/out/" "$HYPERGAS_DIR/out/cf/out-$1/"
}
