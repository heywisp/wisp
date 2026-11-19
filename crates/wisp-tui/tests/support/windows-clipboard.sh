#!/bin/sh
d="${0%/*}"
if [ "$1" != -Sta ]; then
    /bin/cat >> "$d/fallback"
    [ ! -e "$d/fail" ]
    exit
fi
echo "$$" >> "$d/helpers"
if [ -e "$d/dead" ]; then exit 0; fi
while [ -e "$d/startup" ]; do /bin/sleep 0.02; done
while IFS= read -r frame; do
    case "$frame" in *'!') ;; *) exit 1 ;; esac
    if [ -e "$d/bad" ]; then printf 'x'; exit 0; fi
    while [ -e "$d/hold" ]; do /bin/sleep 0.02; done
    printf '%s\n' "${frame%!}" >> "$d/windows-copied"
    printf '\006'
done
