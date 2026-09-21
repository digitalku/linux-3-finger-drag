#!/usr/bin/env bash
# Reverses everything install.sh puts on the system.
#
#   sudo ./uninstall.sh [--keep-config] [--remove-group]
#
#   --keep-config    leave ~/.config/linux-3-finger-drag alone
#   --remove-group   also drop your 'input' group membership
#
# The 'input' group is NOT touched by default: other input tools
# (gesture daemons, remappers, emulators) commonly need it too, and
# taking it away breaks them silently. Opt in only if this program was
# the reason you joined it.

set -u

KEEP_CONFIG=0
REMOVE_GROUP=0
for arg in "$@"; do
    case $arg in
        --keep-config)  KEEP_CONFIG=1 ;;
        --remove-group) REMOVE_GROUP=1 ;;
        -h|--help)      sed -n '2,13p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *)              echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [[ $EUID -ne 0 ]]; then
    echo "This script needs root; it removes files under /etc and /usr/bin."
    echo "Re-run as: sudo $0 $*"
    exit 1
fi

if [[ -z ${SUDO_USER:-} || $SUDO_USER == root ]]; then
    echo "Can't tell whose config to clean up (SUDO_USER is unset)."
    echo "Run this with sudo from your normal user account."
    exit 1
fi

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
USER_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
RUNTIME_DIR="/run/user/$(id -u "$SUDO_USER")"

ok()   { echo -e "[\e[0;32m DONE \e[0m]"; }
skip() { echo -e "[\e[0;33m SKIP \e[0m]"; }
warn() { echo -e "[\e[0;33m WARN \e[0m]"; }

as_user() { su "$SUDO_USER" -c "XDG_RUNTIME_DIR=$RUNTIME_DIR $1"; }

# 1. The service goes first: it holds the running binary (and, on 2.x,
#    an exclusive grab on the trackpad). Stop it before its binary
#    disappears underneath it.
echo -n "Stopping and disabling the service...           "
if [[ -d /run/systemd/system ]]; then
    as_user "systemctl --user disable --now three-finger-drag.service" > /dev/null 2>&1
    rm -f "$USER_HOME/.config/systemd/user/three-finger-drag.service"
    as_user "systemctl --user daemon-reload" > /dev/null 2>&1
    ok
else
    skip
    echo "    No systemd here. If you launched the binary yourself, stop it now."
fi

# 2. The binary
echo -n "Removing the binary...                          "
if [[ -f /usr/bin/linux-3-finger-drag ]]; then
    rm -f /usr/bin/linux-3-finger-drag && ok
else
    skip
fi

# 3. udev rule. The filename is generic enough that something else could
#    own it, so only remove it if it still matches the copy in this repo.
echo -n "Removing the udev rule...                       "
INSTALLED_RULE=/etc/udev/rules.d/60-uinput.rules
if [[ ! -f $INSTALLED_RULE ]]; then
    skip
elif [[ -f $SCRIPT_DIR/60-uinput.rules ]] && ! cmp -s "$SCRIPT_DIR/60-uinput.rules" "$INSTALLED_RULE"; then
    warn
    echo "    $INSTALLED_RULE differs from this repo's copy -- left in place."
    echo "    Something else may own it now. Check it before deleting by hand."
else
    rm -f "$INSTALLED_RULE"
    udevadm control --reload-rules > /dev/null 2>&1
    ok
fi

# 4. uinput autoload. Same caution: only remove the file if it is still
#    exactly what install.sh wrote (a lone "uinput" line).
echo -n "Removing the uinput module-load config...       "
MODCONF=/etc/modules-load.d/uinput.conf
if [[ ! -f $MODCONF ]]; then
    skip
elif [[ $(tr -d '[:space:]' < "$MODCONF") == "uinput" ]]; then
    rm -f "$MODCONF" && ok
else
    warn
    echo "    $MODCONF has extra entries -- left in place so nothing else breaks."
fi

# 5. User config
echo -n "Removing the config directory...                "
CONF_DIR="$USER_HOME/.config/linux-3-finger-drag"
if [[ $KEEP_CONFIG -eq 1 ]]; then
    skip
    echo "    Kept $CONF_DIR (--keep-config)."
elif [[ -d $CONF_DIR ]]; then
    rm -rf "$CONF_DIR" && ok
else
    skip
fi

# 6. Group membership -- opt-in only, see the header.
echo -n "Removing 'input' group membership...            "
if [[ $REMOVE_GROUP -eq 1 ]]; then
    if id -nG "$SUDO_USER" | tr ' ' '\n' | grep -qx input; then
        gpasswd --delete "$SUDO_USER" input > /dev/null && ok
        echo "    Log out and back in for this to take effect."
    else
        skip
    fi
else
    skip
    echo "    Left alone. Pass --remove-group if nothing else needs it."
fi

# Anything installed by hand that install.sh never placed.
if [[ -f /etc/udev/rules.d/61-hotplug.rules ]]; then
    echo
    echo -e "\e[0;33mNote:\e[0m /etc/udev/rules.d/61-hotplug.rules exists."
    echo "install.sh never installs it, so you added it manually. Remove it"
    echo "yourself if it was for this program."
fi

echo
echo -e "\e[0;32mUninstalled.\e[0m The trackpad is back to its normal behaviour"
echo "immediately -- no reboot needed."
