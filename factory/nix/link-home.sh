# Links a user's files into their home from the store, run as root or as the user, and removes
# links an earlier generation made that are gone now. A file that is there and wasn't linked by
# this script is left alone.
# When the user's systemd manager runs, units in ~/.config/systemd/user that went away are
# stopped and the ones that changed are restarted.
#
# Usage: link-home.sh USER GROUP HOME LINKS
# LINKS has a line `~/relative/path<TAB>target` per file, the target in the store, or where the
# machine fills in a file with secrets in it.
set -u
user=$1 group=$2 home=$3 links=$4
manifest=$home/.local/state/distro/home-files
units=$home/.config/systemd/user

[ -d "$home" ] || {
    echo "$user has no home at $home, so its files aren't linked" >&2
    exit 0
}

manager=()
if systemctl is-active --quiet "user@$(id -u "$user").service"; then
    # Run as the user themselves, for a home of its own, their manager is their own.
    if [ "$(id -u)" = "$(id -u "$user")" ]; then
        # Outside a login session, such as under runuser, the manager is still where it runs.
        export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
        manager=(systemctl --user)
    else
        manager=(systemctl --user --machine="$user@")
    fi
fi

# Creates `dir` and the directories above it, up to the home, owned by the user.
make_dirs() {
    local dir=$1 missing=()
    while [ ! -d "$dir" ]; do
        missing=("$dir" "${missing[@]}")
        dir=$(dirname "$dir")
    done
    for dir in "${missing[@]}"; do
        mkdir "$dir"
        chown "$user:$group" "$dir"
    done
}

linked_before() {
    [ -f "$manifest" ] && grep -qxF -- "$1" "$manifest"
}

# Whether `dest` may be (re)linked: nothing is there, or a link this script made or that
# already points at `target`.
may_link() {
    local path=$1 dest=$2 target=$3
    if [ -L "$dest" ]; then
        linked_before "$path" || [ "$(readlink "$dest")" = "$target" ]
    else
        ! [ -e "$dest" ]
    fi
}

# The unit a path names, when it is a user unit file rather than a `.wants` link.
unit_of() {
    [[ $1 == "$units"/* && $1 != "$units"/*/* ]] && basename "$1"
}

wanted=$(cut -f1 "$links")
stopped=()
if [ -f "$manifest" ]; then
    while IFS= read -r path; do
        grep -qxF -- "$path" <<<"$wanted" && continue
        dest=$home/${path#\~/}
        linked=$(readlink "$dest")
        if [ -L "$dest" ] && [[ $linked == /nix/store/* || $linked == /run/distro/templates/* ]]
        then
            unit=$(unit_of "$dest") && stopped+=("$unit")
            rm "$dest"
        fi
    done <"$manifest"
fi

made=() changed=()
while IFS=$'\t' read -r path target; do
    dest=$home/${path#\~/}
    if ! may_link "$path" "$dest" "$target"; then
        echo "leaving $dest alone: it is there and distro didn't link it" >&2
        continue
    fi
    if [ "$(readlink "$dest")" != "$target" ]; then
        unit=$(unit_of "$dest") && changed+=("$unit")
    fi
    make_dirs "$(dirname "$dest")"
    ln -sfn "$target" "$dest"
    chown -h "$user:$group" "$dest"
    made+=("$path")
done <"$links"

make_dirs "$(dirname "$manifest")"
printf '%s\n' "${made[@]}" | grep -v '^$' >"$manifest.next" || true
chown "$user:$group" "$manifest.next"
mv "$manifest.next" "$manifest"

if [ ${#manager[@]} -gt 0 ]; then
    [ ${#stopped[@]} -gt 0 ] && "${manager[@]}" --no-block stop "${stopped[@]}"
    "${manager[@]}" daemon-reload
    for unit in "${changed[@]}"; do
        # A unit a target wants starts or restarts; one started by hand restarts if running.
        if compgen -G "$units/*.wants/$unit" >/dev/null; then
            "${manager[@]}" --no-block restart "$unit"
        else
            "${manager[@]}" --no-block try-restart "$unit"
        fi
    done
fi
