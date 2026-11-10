#!/bin/sh
# har-browser-cog.sh -- inside the Debian root, as user browser (run-browser-rb3.sh): runs Cog
# and tells the dashboard how its page is doing, for the loader over the browser
# (smarthome-app browser.rs BrowserEvent). Cog's output still goes to its log; from it one
# word a line goes to the FIFO /run/har/har-browser-events:
#
#   "<url> Loading..."             -> committed   (the first answer replaced the old page)
#   "<url> Loaded successfully."   -> loaded
#   "<url> <what>: <why>"          -> failed      (Cog's load-failed and TLS messages)
#
# about:blank (the dashboard's own "empty") says nothing. Only words, never the URL: the app
# takes three known words and drops anything else. Without the FIFO Cog runs as before.
FIFO=/run/har/har-browser-events
LOG=/home/browser/cog.log
: > "$LOG"
# tested first: a redirection that fails ends a POSIX shell, an || after it never runs
[ -p "$FIFO" ] || exec cog --platform=wl "$@" >>"$LOG" 2>&1
exec 3<>"$FIFO"
cog --platform=wl "$@" 2>&1 | while IFS= read -r line; do
  printf '%s\n' "$line" >>"$LOG"
  case "$line" in
    *"<about:blank> "*) ;;
    *"> Loading...") echo committed >&3 ;;
    *"> Loaded successfully.") echo loaded >&3 ;;
    *"> Load started.") ;;
    "Cog-Core-Message:"*"> "*": "*) echo failed >&3 ;;
  esac
done
