#!/bin/bash

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Faithful reproduction of Wheel's process-backend engine drop, to settle 037's /proc carrier.
# Mirrors crates/wheel-host/src/sandbox/process.rs::drop_privileges:
#   setgroups([]) -> setgid -> setuid -> PR_SET_NO_NEW_PRIVS, then execve a normal binary.
# setpriv --clear-groups --regid --reuid --no-new-privs does exactly that sequence.
set -u
UID_ENG=21088   # the shared project uid (PM's production uid)
UID_OTHER=21089 # a different uid, to show the boundary

echo "=== kernel context ==="
uname -r
echo "suid_dumpable = $(cat /proc/sys/fs/suid_dumpable)"
echo "hidepid on /proc: $(grep ' /proc ' /proc/mounts || true)"
echo

echo "=== launch the 'engine' as uid $UID_ENG with secrets in its environ (faithful drop) ==="
# env sets the secrets in root's env; setpriv drops creds then execve(sleep), carrying them into environ.
env WHEEL_ENGINE_SECRET='ENGINE-BEARER-godmode' WHEEL_VAULT_KEY='VAULTKEY-decrypts-all' \
    setpriv --clear-groups --regid "$UID_ENG" --reuid "$UID_ENG" --no-new-privs \
    sleep 3000 &
ENG=$!
sleep 0.5
echo "engine pid=$ENG  creds: $(grep -E '^(Uid|Gid)' /proc/$ENG/status | tr '\n' ' ')"
echo "owner+mode of /proc/$ENG/environ:"
ls -l /proc/$ENG/environ 2>&1 || true
echo

echo "=== TEST 1: SAME-UID sibling ($UID_ENG) reads the engine's environ (the 037 claim) ==="
setpriv --clear-groups --regid "$UID_ENG" --reuid "$UID_ENG" --no-new-privs \
    sh -c "tr '\0' '\n' < /proc/$ENG/environ | grep -E 'WHEEL_(ENGINE_SECRET|VAULT_KEY)'" \
    && echo ">>> RESULT 1: SAME-UID SIBLING READ THE SECRETS  (037 CONFIRMED)" \
    || echo ">>> RESULT 1: same-uid sibling was DENIED  (037 /proc carrier CLOSED)"
echo

echo "=== TEST 2: DIFFERENT-UID process ($UID_OTHER) reads the engine's environ (boundary check) ==="
setpriv --clear-groups --regid "$UID_OTHER" --reuid "$UID_OTHER" --no-new-privs \
    sh -c "tr '\0' '\n' < /proc/$ENG/environ | grep -E 'WHEEL_' " \
    && echo ">>> RESULT 2: different uid READ the secrets (no isolation at all)" \
    || echo ">>> RESULT 2: different uid DENIED (expected — uid is the boundary)"
echo

echo "=== TEST 3: same-uid sibling reads /proc/<engine>/mem-adjacent: cmdline + status (baseline) ==="
setpriv --clear-groups --regid "$UID_ENG" --reuid "$UID_ENG" --no-new-privs \
    sh -c "head -c 60 /proc/$ENG/cmdline | tr '\0' ' '; echo" \
    && echo ">>> RESULT 3: cmdline readable (expected; cmdline is world-readable)"

kill "$ENG" 2>/dev/null
echo "=== done ==="
