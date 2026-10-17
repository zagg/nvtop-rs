#!/bin/sh
# tests/compare.sh — recoupe nvtop-rs avec la reference proprietaire.
#
#   sh tests/compare.sh            # ou avec un binaire force :
#   BIN=./build/nvtop-rs sh tests/compare.sh
#
# Le tool lit le pilote sans NVML ; ce script verifie que les nombres produits
# sont bien ceux que nvidia-smi affiche (a l'arrondi pres). Sortie 0 = tout OK.
BIN=${BIN:-./target/release/nvtop-rs}
GPU=${GPU:-0}

command -v nvidia-smi >/dev/null 2>&1 || { echo "nvidia-smi absent : test ignore"; exit 0; }
[ -x "$BIN" ] || { echo "binaire manquant : $BIN (make release)"; exit 2; }

fail=0
check() { # nom, attendu, obtenu
    if [ "$2" = "$3" ]; then
        printf '  \033[32mPASS\033[0m %-22s %s\n' "$1" "$2"
    else
        printf '  \033[31mFAIL\033[0m %-22s attendu=%s obtenu=%s\n' "$1" "$2" "$3"
        fail=$((fail + 1))
    fi
}
near() { # nom, attendu, obtenu, tolerance
    awk -v a="$2" -v b="$3" -v t="$4" 'BEGIN{ x=a-b; if(x<0)x=-x; exit (x<=t)?0:1 }' \
      && { printf '  \033[32mPASS\033[0m %-22s %s (~%s)\n' "$1" "$3" "$2"; } \
      || { printf '  \033[31mFAIL\033[0m %-22s attendu=%s obtenu=%s\n' "$1" "$2" "$3"; fail=$((fail+1)); }
}

q()  { nvidia-smi -i "$GPU" --query-gpu="$1" --format=csv,noheader,nounits | tr -d ' \r'; }
qw() { nvidia-smi -i "$GPU" --query-gpu="$1" --format=csv,noheader,nounits | tr -d '\r'; } # garde les espaces
j() { "$BIN" --json --gpu "$GPU" | python3 -c "import json,sys;d=json.load(sys.stdin)['gpus'][0];print($1)"; }

echo "== nvtop-rs vs nvidia-smi (GPU $GPU) =="
check   "name"              "$(qw name)"                      "$(j "d['name']")"
check   "memory.total"      "$(q memory.total)"               "$(j "d['memory_mib']['total']")"
check   "memory.used"       "$(q memory.used)"                "$(j "d['memory_mib']['used']")"
check   "memory.free"       "$(q memory.free)"                "$(j "d['memory_mib']['free']")"
near    "temperature.gpu"   "$(q temperature.gpu)"            "$(j "d['temperature_c']['gpu']")" 1.5
near    "power.draw"        "$(q power.draw)"                 "$(j "d['power_w']['average']")" 2.0
near    "power.limit"       "$(q power.limit)"                "$(j "d['power_w']['limit']")" 0.6
check   "pstate"            "$(q pstate)"                     "P$(j "int(d['pstate'][1:])")"
check   "pcie gen"          "$(q pcie.link.gen.current)"      "$(j "d['pcie']['gen_current']")"
check   "pcie width"        "$(q pcie.link.width.current)"    "$(j "d['pcie']['width_current']")"
check   "vbios"             "$(q vbios_version)"              "$(j "d['vbios']")"
check   "uuid"              "$(q gpu_uuid)"                   "$(j "d['uuid']")"
near    "clocks.sm"         "$(q clocks.current.sm)"          "$(j "d['clocks_mhz']['sm']")" 30
near    "clocks.max.sm"     "$(q clocks.max.sm)"              "$(j "d['clocks_mhz']['max_graphics']")" 1
check   "driver_version"    "$(q driver_version)"             "$("$BIN" --json | python3 -c "import json,sys;print(json.load(sys.stdin)['driver_version'])")"

echo
echo "== ce que nvidia-smi ne sait pas dire (donc rien a comparer) =="
"$BIN" --json | python3 -c "
import json,sys
d=json.load(sys.stdin)['gpus'][0]
for k in ('pma_bytes','throttle','bar1_mib'):
    print('  %-22s %s' % (k, json.dumps(d[k])[:100]))
print('  %-22s %s' % ('fan.rpm', d['fan']['rpm']))
print('  %-22s %s' % ('power.module/tgpm', (d['power_w']['module'], d['power_w']['tgpm'])))
"
[ "$fail" = 0 ] && echo && echo "tout correspond." || { echo; echo "$fail champ(s) en desaccord"; exit 1; }
