#!/bin/sh
# har-browser-relay.sh -- inside the Debian root, as user browser (everyware-browser.sh starts
# it beside Cog, or run-browser-rb3.sh for a developer's build): the pages the dashboard asks for, one URL a line in the FIFO
# /run/har/har-browser-open, go to Cog with cogctl over the root's own D-Bus session bus.
#
# The dashboard (smarthome-app browser.rs) sends only http, https, about:blank and the word
# restart (see below), one line,
# no spaces or control characters; this checks the same again, so nothing written to the FIFO
# makes Cog open file:, data: or javascript: pages. URLs are not logged (they are the
# household's reading).
FIFO=/run/har/har-browser-open
echo $$ > /tmp/har-browser-relay.pid
# Read-write: the relay holds a writer itself, so it never sees EOF between two messages and
# a page sent while it would reopen the FIFO is not lost.
exec 3<>"$FIFO" || exit 1
while IFS= read -r url <&3; do
  # The one word that is not an address: the dashboard had no word from the page it asked
  # for (browser.rs restart_pages). A page process of WebKit can hang for good, and then
  # every later page waits behind it. This user's page processes are ended; Cog starts new
  # ones for the page the dashboard sends right after.
  if [ "$url" = restart ]; then
    for p in /proc/[0-9]*; do
      [ "$(cat "$p/comm" 2>/dev/null)" = WPEWebProcess ] && kill -9 "${p#/proc/}" 2>/dev/null
    done
    echo "ended the page processes"
    # Cog loads its own page for the ended renderer first; the next page must come after it
    sleep 1
    continue
  fi
  case "$url" in
    # the scheme in any case, as the app takes it (browser.rs is_web)
    about:blank|[Hh][Tt][Tt][Pp]://*|[Hh][Tt][Tt][Pp][Ss]://*) ;;
    *) echo "refused: not a web address"; continue ;;
  esac
  case "$url" in
    *[[:space:]]*|*[[:cntrl:]]*) echo "refused: a space or a control character"; continue ;;
  esac
  [ "${#url}" -le 2048 ] || { echo "refused: too long"; continue; }
  if cogctl open "$url"; then echo "opened a page (${#url} bytes)"; else echo "cogctl failed"; fi
done
