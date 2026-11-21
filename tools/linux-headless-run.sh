#!/bin/bash
# usage: run.sh [seconds]   (as neo)  headless leaf in a private kwin --virtual
SECS=${1:-30}
BASE=/tmp/leafrun-$$
mkdir -p $BASE/{rt,cfg,data,cache,home,folder} && chmod 700 $BASE $BASE/rt
for i in a b c; do echo hi > $BASE/folder/$i.txt; done; mkdir $BASE/folder/sub
LOG=/home/neo/Documents/Claude/leaf/run1
export -n DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
E="env -i LEAF_RUST_LOG=$LEAF_RUST_LOG LEAF_WLDEBUG=$LEAF_WLDEBUG PATH=/usr/bin:/bin HOME=$BASE/home XDG_RUNTIME_DIR=$BASE/rt XDG_CONFIG_HOME=$BASE/cfg XDG_DATA_HOME=$BASE/data XDG_CACHE_HOME=$BASE/cache"
timeout $((SECS+25)) $E dbus-run-session -- bash -c '
 kwin_wayland --virtual --socket leafrun-0 --no-lockscreen --width 1280 --height 800 > '$LOG'/kwin.log 2>&1 &
 K=$!
 for i in $(seq 50); do [ -S $XDG_RUNTIME_DIR/leafrun-0 ] && break; sleep 0.2; done
 ls $XDG_RUNTIME_DIR
 (sleep 8; WAYLAND_DISPLAY=leafrun-0 QT_QPA_PLATFORM=wayland timeout 15 spectacle -b -f -n -o /home/neo/Documents/Claude/leaf/run1/shot.png > /home/neo/Documents/Claude/leaf/run1/spectacle.log 2>&1) &
 WAYLAND_DISPLAY=leafrun-0 LEAF_CONFIG_DIR='$BASE'/cfg RUST_BACKTRACE=1 RUST_LOG=${LEAF_RUST_LOG:-warn} WAYLAND_DEBUG=${LEAF_WLDEBUG:-0} \
   timeout '$SECS' /home/neo/Documents/Claude/leaf/run1/target/debug/leaf '$BASE'/folder > '$LOG'/leaf.log 2>&1
 echo leaf exit $? >> '$LOG'/leaf.log
 kill $K; wait $K 2>/dev/null; sleep 1
'
rm -rf $BASE
