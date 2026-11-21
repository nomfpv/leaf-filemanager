#!/bin/bash
# usage: run.sh [seconds]   (as neo)  headless leaf in a private kwin --virtual
# env: VW VH (screen size), BIN (leaf binary), DEMO (folder copied in as the shown folder), OPEN (name inside it to start with),
#      SETTINGS (a settings.toml for the throwaway config), LEAF_RUST_LOG, LEAF_WLDEBUG, LEAF_TRACE_START
SECS=${1:-30}
BASE=/tmp/leafrun-$$
mkdir -p $BASE/{rt,cfg,data,cache,home,folder} && chmod 700 $BASE $BASE/rt
for i in a b c; do echo hi > $BASE/folder/$i.txt; done; mkdir $BASE/folder/sub
[ -n "$DEMO" ] && { rm -rf $BASE/folder; cp -r "$DEMO" $BASE/folder; }
[ -n "$SETTINGS" ] && cp "$SETTINGS" $BASE/cfg/settings.toml
BIN=${BIN:-/home/neo/Documents/Claude/leaf/run1/target/debug/leaf}
LOG=/home/neo/Documents/Claude/leaf/run1
P=/home/neo/Documents/Claude/leaf/run1/src/packaging/linux
if [ -d $P ]; then mkdir -p $BASE/data/applications $BASE/data/icons/hicolor/256x256/apps; cp $P/leaf.desktop $BASE/data/applications/; cp $P/icons/leaf-256.png $BASE/data/icons/hicolor/256x256/apps/leaf.png; fi
export -n DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
E="env -i $([ -n "$LEAF_RUST_LOG" ] && echo RUST_LOG=$LEAF_RUST_LOG) LEAF_RUST_LOG=$LEAF_RUST_LOG LEAF_WLDEBUG=$LEAF_WLDEBUG $([ -n "$LEAF_TRACE_START" ] && echo LEAF_TRACE_START=1) LANG=en_US.UTF-8 PATH=/usr/bin:/bin HOME=$BASE/home XDG_RUNTIME_DIR=$BASE/rt XDG_CONFIG_HOME=$BASE/cfg XDG_DATA_HOME=$BASE/data XDG_CACHE_HOME=$BASE/cache"
timeout $((SECS+25)) $E dbus-run-session -- bash -c '
 kwin_wayland --virtual --socket leafrun-0 --no-lockscreen --width '${VW:-1280}' --height '${VH:-800}' > '$LOG'/kwin.log 2>&1 &
 K=$!
 for i in $(seq 50); do [ -S $XDG_RUNTIME_DIR/leafrun-0 ] && break; sleep 0.2; done
 ls $XDG_RUNTIME_DIR
 (sleep 8; WAYLAND_DISPLAY=leafrun-0 QT_QPA_PLATFORM=wayland timeout 15 spectacle -b -f -n -o /home/neo/Documents/Claude/leaf/run1/shot.png > /home/neo/Documents/Claude/leaf/run1/spectacle.log 2>&1) &
 WAYLAND_DISPLAY=leafrun-0 LEAF_CONFIG_DIR='$BASE'/cfg RUST_BACKTRACE=1 WAYLAND_DEBUG=${LEAF_WLDEBUG:-0} \
   timeout '$SECS' '$BIN' '$BASE'/folder'${OPEN:+/$OPEN}' > '$LOG'/leaf.log 2>&1
 echo leaf exit $? >> '$LOG'/leaf.log
 kill $K; wait $K 2>/dev/null; sleep 1
'
rm -rf $BASE
