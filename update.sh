#!/usr/bin/env bash
# Updates an existing install to the latest version of this repo.
#
#   sudo ./update.sh [--no-pull]
#
#   --no-pull    build and install this checkout as it is, without
#                fetching anything first (e.g. to try local changes)
#
# Unlike install.sh, this leaves your config and group membership alone
# and never needs a reboot: it pulls, rebuilds, swaps the binary, and
# restarts the service. The systemd unit and udev rule are refreshed too,
# but only if your installed copies haven't been edited by hand.

set -u

BIN=/usr/bin/linux-3-finger-drag
SERVICE=three-finger-drag.service
RULE=60-uinput.rules

ok()   { echo -e "[\e[0;32m DONE \e[0m]"; }
skip() { echo -e "[\e[0;33m SKIP \e[0m]"; }
warn() { echo -e "[\e[0;33m WARN \e[0m]"; }
fail() { echo -e "[\e[0;31m FAIL \e[0m]"; }

as_user() { su "$SUDO_USER" -c "XDG_RUNTIME_DIR=$RUNTIME_DIR $1"; }

# Was the installed file ($2) ever shipped by this repo as $1? If so, it
# hasn't been customised, and it's safe to replace with the current copy.
shipped_before() {
    local blob
    # hashed from / so root's git doesn't trip over the user-owned repo
    blob=$(cd / && git hash-object --no-filters -- "$2") || return 1
    as_user "cd $REPO_Q && git log --format=%H -- $1 \
        | while read -r c; do git rev-parse -q --verify \"\$c:$1\"; done" 2> /dev/null \
        | grep -qx "$blob"
}

# Replace the installed file ($2) with the repo's copy of $1, unless the
# installed one has local edits. Prints its own status; returns 0 only
# if the file was replaced.
refresh_file() {
    local file=$1 installed=$2
    if [[ ! -f $installed ]]; then
        skip
        echo "    $installed isn't installed."
        return 1
    fi
    if cmp -s "$SCRIPT_DIR/$file" "$installed"; then
        skip
        echo "    Already up to date."
        return 1
    fi
    if shipped_before "$file" "$installed"; then
        if cp "$SCRIPT_DIR/$file" "$installed"; then
            ok
            return 0
        fi
        fail
        return 1
    fi
    warn
    echo "    $installed has local edits -- left alone."
    echo "    Compare it with $file in this repo and merge by hand if needed."
    return 1
}

# Everything runs inside main(), so bash has read the whole script before
# `git pull` gets a chance to rewrite this file underneath it.
main() {
    local pull=1
    for arg in "$@"; do
        case $arg in
            --no-pull) pull=0 ;;
            -h|--help) sed -n '2,12p' "$0" | sed 's/^# \?//'; exit 0 ;;
            *)         echo "Unknown option: $arg" >&2; exit 2 ;;
        esac
    done

    if [[ $EUID -ne 0 ]]; then
        echo "This script needs root; it replaces $BIN."
        echo "Re-run as: sudo $0 $*"
        exit 1
    fi

    if [[ -z ${SUDO_USER:-} || $SUDO_USER == root ]]; then
        echo "Can't tell whose install to update (SUDO_USER is unset)."
        echo "Run this with sudo from your normal user account."
        exit 1
    fi

    if [[ ! -f $BIN ]]; then
        echo "linux-3-finger-drag isn't installed yet ($BIN is missing)."
        echo "Install it first with: sudo bash install.sh"
        exit 1
    fi

    SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    REPO_Q=$(printf '%q' "$SCRIPT_DIR")
    USER_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
    RUNTIME_DIR="/run/user/$(id -u "$SUDO_USER")"

    # 1. Fetch. Fast-forward only: if the checkout has diverged or has
    #    conflicting local changes, stop before anything is replaced.
    echo -n "Fetching the latest version...                  "
    if [[ $pull -eq 0 ]]; then
        skip
        echo "    --no-pull: building this checkout as it is."
    else
        local old_rev new_rev out
        old_rev=$(as_user "git -C $REPO_Q rev-parse HEAD")
        if ! out=$(as_user "git -C $REPO_Q pull --ff-only" 2>&1); then
            fail
            echo "$out" | sed 's/^/    /'
            echo "    Nothing was changed. Commit or stash your local changes,"
            echo "    or pass --no-pull to install this checkout as it is."
            exit 1
        fi
        new_rev=$(as_user "git -C $REPO_Q rev-parse HEAD")
        ok
        if [[ $old_rev == "$new_rev" ]]; then
            echo "    Already on the latest commit."
        else
            as_user "git -C $REPO_Q log --oneline $old_rev..$new_rev" | sed 's/^/    /'
        fi
    fi

    # 2. Build as the user: cargo shouldn't run as root, and a login shell
    #    puts ~/.cargo/bin on the PATH.
    echo "Compiling..."
    echo
    if ! su -l "$SUDO_USER" -c "cd $REPO_Q && cargo build --release"; then
        echo
        echo -e "\e[0;31mBuild failed\e[0m -- the installed version is untouched and still running."
        exit 1
    fi
    echo

    # 3. Binary. `install` removes the old file before writing the new one,
    #    so this works while the service is still running the old binary
    #    (copying over it with `cp` fails with "Text file busy").
    local binary_changed=0
    echo -n "Installing the binary...                        "
    if cmp -s "$SCRIPT_DIR/target/release/linux-3-finger-drag" "$BIN"; then
        skip
        echo "    The installed binary is already this version."
    else
        install -m 755 "$SCRIPT_DIR/target/release/linux-3-finger-drag" "$BIN" || { fail; exit 1; }
        binary_changed=1
        ok
    fi

    # 4. systemd user unit
    local service_changed=0
    echo -n "Refreshing the systemd user unit...             "
    if refresh_file "$SERVICE" "$USER_HOME/.config/systemd/user/$SERVICE"; then
        as_user "systemctl --user daemon-reload" > /dev/null 2>&1
        service_changed=1
    fi

    # 5. udev rule
    echo -n "Refreshing the udev rule...                     "
    if refresh_file "$RULE" "/etc/udev/rules.d/$RULE"; then
        udevadm control --reload-rules > /dev/null 2>&1
        udevadm trigger --sysname-match=uinput > /dev/null 2>&1
        echo "    If the trackpad stops responding, log out and back in."
    fi

    # 6. Restart, so the new binary is what's actually running
    echo -n "Restarting the service...                       "
    if [[ $binary_changed -eq 0 && $service_changed -eq 0 ]]; then
        skip
        echo "    Nothing changed."
    elif [[ ! -d /run/systemd/system ]]; then
        skip
        echo "    No systemd here. Restart linux-3-finger-drag yourself"
        echo "    (logging out and back in will do it)."
    elif as_user "systemctl --user is-active --quiet $SERVICE"; then
        as_user "systemctl --user restart $SERVICE" && ok
    else
        skip
        echo "    The service isn't running; the new version is used next time it starts."
    fi

    echo
    echo -e "\e[0;32mUpdate complete!\e[0m No reboot needed."
    echo "Your config in $USER_HOME/.config/linux-3-finger-drag was left as it is;"
    echo "any new options take their default values until you set them."
}

main "$@"; exit $?
