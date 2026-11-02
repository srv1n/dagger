#!/bin/bash
# Interleaved A/B of two engine_cpu binaries: alternates runs so both see the same machine load.
# usage: scripts/ab_engine_cpu.sh OLD_BIN NEW_BIN [ROUNDS]
set -euo pipefail
old=$1; new=$2; rounds=${3:-5}
for i in $(seq "$rounds"); do
  for arm in old new; do
    bin=$old; [ "$arm" = new ] && bin=$new
    out=$( { /usr/bin/time -p "$bin" > /tmp/ab_run.$$ ; } 2>&1 )
    user=$(awk '/^user/{print $2}' <<<"$out"); sys=$(awk '/^sys/{print $2}' <<<"$out")
    cached=$(awk '/^cached:/{print $2}' /tmp/ab_run.$$ | tr -d s); cold=$(awk '/^cold:/{print $2}' /tmp/ab_run.$$ | tr -d s)
    echo "$arm user=$user sys=$sys cold=$cold cached=$cached"
  done
done | tee /dev/stderr | awk '{for(i=2;i<=NF;i++){split($i,kv,"="); v[$1,kv[1]]=v[$1,kv[1]]" "kv[2]}} END {
  split("user sys cold cached",ks," "); for(a in arms); 
  for(k=1;k<=4;k++) for(arm=0;arm<2;arm++){ name=(arm?"new":"old"); n=split(v[name,ks[k]],x," "); asort_cmd="";
    # median
    for(i=1;i<=n;i++) for(j=i+1;j<=n;j++) if(x[j]<x[i]){t=x[i];x[i]=x[j];x[j]=t}
    printf "%s %-6s median=%s\n", name, ks[k], x[int((n+1)/2)] } }'
rm -f /tmp/ab_run.$$
