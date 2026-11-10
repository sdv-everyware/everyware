# measure.sh WINDOW_S: CPU % (100 = one full core) and memory per process over a window (read-only)
W=${1:-10}
PIDS="$(pidof smarthome_zone) $(pidof cog) $(pidof WPEWebProcess) $(pidof WPENetworkProcess)"
snap() { for p in $PIDS; do echo "$p $(cut -d')' -f2 /proc/$p/stat | awk '{print $12+$13}')"; done; }
snap > /data/local/tmp/m0.txt
c0=$(awk '/^cpu /{t=0; for(i=2;i<=8;i++) t+=$i; print t, $5+$6}' /proc/stat)
sleep $W
snap > /data/local/tmp/m1.txt
c1=$(awk '/^cpu /{t=0; for(i=2;i<=8;i++) t+=$i; print t, $5+$6}' /proc/stat)
echo "process          cpu%   RSS_MB  PSS_MB"
for p in $PIDS; do
  b=$(awk -v p=$p '$1==p{print $2}' /data/local/tmp/m0.txt); e=$(awk -v p=$p '$1==p{print $2}' /data/local/tmp/m1.txt)
  rss=$(awk '/^VmRSS/{printf "%d", $2/1024}' /proc/$p/status)
  pss=$(awk '/^Pss:/{printf "%d", $2/1024}' /proc/$p/smaps_rollup 2>/dev/null)
  printf "%-16s %5s %8s %7s\n" "$(cat /proc/$p/comm)" "$(awk -v b=$b -v e=$e -v w=$W 'BEGIN{printf "%.1f", (e-b)/w}')" "$rss" "$pss"
done
set -- $c0; t0=$1; i0=$2; set -- $c1
echo "system busy: $(( 100 - ($2 - i0) * 100 / ($1 - t0) ))% of 8 cores; $(awk '/MemTotal|MemAvailable/{printf "%s %d MB  ", $1, $2/1024}' /proc/meminfo)"
