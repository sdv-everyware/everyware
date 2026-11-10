#!/bin/sh
# har-browser-relay.sh -- inside the Debian root, as user browser (everyware-browser.sh starts
# it beside Cog, or run-browser-rb3.sh for a developer's build): the pages the dashboard asks for, one URL a line in the FIFO
# /run/har/har-browser-open, go to Cog with cogctl over the root's own D-Bus session bus.
#
# The dashboard (smarthome-app browser.rs) sends only http, https and about:blank, one line,
# no spaces or control characters; this checks the same again, so nothing written to the FIFO
# makes Cog open file:, data: or javascript: pages. URLs are not logged (they are the
# household's reading).
FIFO=/run/har/har-browser-open
echo $$ > /tmp/har-browser-relay.pid
# Read-write: the relay holds a writer itself, so it never sees EOF between two messages and
# a page sent while it would reopen the FIFO is not lost.
exec 3<>"$FIFO" || exit 1
while IFS= read -r url <&3; do
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
