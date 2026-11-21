#!/bin/bash
# usage: run.sh [seconds]   (as neo)  headless leaf in a private kwin --virtual
# env: VW VH (screen size), BIN (leaf binary), DEMO (folder copied in as the shown folder), OPEN (name inside it to start with),
#      SETTINGS (a settings.toml for the throwaway config), LEAF_RUST_LOG, LEAF_WLDEBUG, LEAF_TRACE_START
#      SCRIPT (a LEAF_CHECK_SCRIPT file, see tools/TESTING.md: $FOLDER, $OTHER (an empty second folder), $DATA (the private
#      XDG_DATA_HOME, its Trash is the throwaway trash) and $BASE are replaced by the temp paths), SHOTS (where the `shot` lines'
#      pictures go, default run1/shots; with SCRIPT the one picture after 8 s is not taken, the script ends leaf with `quit`; then
#      SECS is only the longest it may run), SHELL_AFTER (a shell command run after leaf ended, with $BASE still there)
ROOT=${ROOT:-/home/neo/Documents/Claude/leaf/run1}   # the build copy: $ROOT/target/debug/leaf, $ROOT/src, logs and pictures go here
SECS=${1:-30}
BASE=/tmp/leafrun-$$
mkdir -p $BASE/{rt,cfg,data,cache,home,folder,other} && chmod 700 $BASE $BASE/rt
for i in a b c; do echo hi > $BASE/folder/$i.txt; done; mkdir $BASE/folder/sub
[ -n "$DEMO" ] && { rm -rf $BASE/folder; cp -r "$DEMO" $BASE/folder; }
[ -n "$SETTINGS" ] && cp "$SETTINGS" $BASE/cfg/settings.toml
SHOTS=${SHOTS:-$ROOT/shots}
if [ -n "$SCRIPT" ]; then
  mkdir -p "$SHOTS"; rm -f "$SHOTS"/*.png
  sed "s|\$FOLDER|$BASE/folder|g; s|\$OTHER|$BASE/other|g; s|\$DATA|$BASE/data|g; s|\$BASE|$BASE|g" "$SCRIPT" > $BASE/script.txt
fi
BIN=${BIN:-$ROOT/target/debug/leaf}
LOG=$ROOT
P=$ROOT/src/packaging/linux
if [ -d $P ]; then mkdir -p $BASE/data/applications $BASE/data/icons/hicolor/256x256/apps; cp $P/leaf.desktop $BASE/data/applications/; cp $P/icons/leaf-256.png $BASE/data/icons/hicolor/256x256/apps/leaf.png; fi
export -n DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
E="env -i $([ -n "$LEAF_RUST_LOG" ] && echo RUST_LOG=$LEAF_RUST_LOG) LEAF_RUST_LOG=$LEAF_RUST_LOG LEAF_WLDEBUG=$LEAF_WLDEBUG $([ -n "$SCRIPT" ] && echo LEAF_CHECK_SCRIPT=$BASE/script.txt LEAF_CHECK_SHOTDIR=$SHOTS) $([ -n "$LEAF_TRACE_START" ] && echo LEAF_TRACE_START=1) LANG=en_US.UTF-8 PATH=/usr/bin:/bin HOME=$BASE/home XDG_RUNTIME_DIR=$BASE/rt XDG_CONFIG_HOME=$BASE/cfg XDG_DATA_HOME=$BASE/data XDG_CACHE_HOME=$BASE/cache"
timeout $((SECS+25)) $E dbus-run-session -- bash -c '
 kwin_wayland --virtual --socket leafrun-0 --no-lockscreen --width '${VW:-1280}' --height '${VH:-800}' > '$LOG'/kwin.log 2>&1 &
 K=$!
 for i in $(seq 50); do [ -S $XDG_RUNTIME_DIR/leafrun-0 ] && break; sleep 0.2; done
 ls $XDG_RUNTIME_DIR
 if [ -z "'$SCRIPT'" ]; then
  (sleep 8; WAYLAND_DISPLAY=leafrun-0 QT_QPA_PLATFORM=wayland timeout 15 spectacle -b -f -n -o '$ROOT'/shot.png > '$ROOT'/spectacle.log 2>&1) &
 else
  # the script asks for a picture with a `LEAF_SHOT name` line on stdout; the picture is moved into place when it is whole
  (while sleep 0.3; do
    for n in $(grep -ao "LEAF_SHOT [A-Za-z0-9_.-]*" '$LOG'/leaf.log | cut -d" " -f2 | sort -u); do
      [ -e '$SHOTS'/$n.png ] || { WAYLAND_DISPLAY=leafrun-0 QT_QPA_PLATFORM=wayland timeout 20 spectacle -b -f -n -o '$SHOTS'/.$n.tmp.png >> '$LOG'/spectacle.log 2>&1; mv '$SHOTS'/.$n.tmp.png '$SHOTS'/$n.png; }
    done
  done) &
  W=$!
 fi
 WAYLAND_DISPLAY=leafrun-0 LEAF_CONFIG_DIR='$BASE'/cfg RUST_BACKTRACE=1 WAYLAND_DEBUG=${LEAF_WLDEBUG:-0} \
   timeout '$SECS' '$BIN' '$BASE'/folder'${OPEN:+/$OPEN}' > '$LOG'/leaf.log 2>&1
 echo leaf exit $? >> '$LOG'/leaf.log
 [ -n "$W" ] && kill $W
 kill $K; wait $K 2>/dev/null; sleep 1
'
[ -n "$SHELL_AFTER" ] && BASE=$BASE bash -c "$SHELL_AFTER"
rm -rf $BASE
