#!/usr/bin/env bash
# Dep-sync gate for the transitional three-grouping workspace. Always
# blocking: the crate DAG points strictly up (nexum <- videre <-
# shepherd) for every workspace member, and path deps and wit/ symlinks
# never cross downward. Agreement checks: the umbrella [patch] table,
# member git-tag pins, per-group Cargo.repo.toml manifests, and
# wit-deps manifests and locks must name the same crates and the same
# one tag per upstream repo. An artefact a train wave has not written
# yet skips visibly and enforces the moment it lands. See
# docs/design/cross-repo-deps.md. Run locally via `just check-dep-sync`.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2
root="$PWD"

pass() { printf '\033[1;32m[dep-sync PASS]\033[0m %s\n' "$*" >&2; }
fail() { printf '\033[1;31m[dep-sync FAIL]\033[0m %s\n' "$*" >&2; status=1; }
skip() { printf '\033[1;33m[dep-sync SKIP]\033[0m %s\n' "$*" >&2; }

command -v rg >/dev/null || { echo "ripgrep (rg) is required" >&2; exit 2; }
command -v cargo >/dev/null || { echo "cargo is required" >&2; exit 2; }

status=0

layers=(nexum videre shepherd)
rank() {
    case $1 in
        nexum) echo 0 ;;
        videre) echo 1 ;;
        shepherd) echo 2 ;;
        *) echo -1 ;;
    esac
}
# Owning grouping per upstream repo; shepherd is app-level, never a
# dependency target.
slug_owner() {
    case $1 in
        nexum-runtime) echo nexum ;;
        videre-nexum-module) echo videre ;;
        *) echo "" ;;
    esac
}

crate_name() { awk -F'"' '/^name = /{print $2; exit}' "$1/Cargo.toml"; }
toml_members() { awk '/^members = \[/{f=1;next} f&&/^\]/{exit} f' "$1" | tr -d ' ",' | rg -v '^$' || true; }
path_dep_values() { rg -o 'path *= *"([^"]+)"' -r '$1' "$1" 2>/dev/null || true; }

mapfile -t members < <(toml_members Cargo.toml)
if [[ ${#members[@]} -eq 0 ]]; then
    fail "no workspace members parsed from Cargo.toml"
    exit 1
fi

# 1. Crate DAG: every member's full build closure (normal, build, and
#    its own dev edges) stays in its own grouping or above. A stale
#    lock fails once here: the lock must move with the manifests.
dag_bad=0
if ! cargo metadata --format-version 1 --locked >/dev/null; then
    fail "Cargo.lock out of sync with the manifests"
    dag_bad=1
fi
[[ $dag_bad -eq 1 ]] || for m in "${members[@]}"; do
    g="${m%%/*}"
    r="$(rank "$g")"
    name="$(crate_name "$m")"
    if [[ -z $name ]]; then
        fail "no crate name in $m/Cargo.toml"
        dag_bad=1
        continue
    fi
    if ! tree="$(cargo tree -p "$name" -e normal,build,dev --all-features --prefix none --locked)"; then
        fail "cargo tree failed for $name ($m)"
        dag_bad=1
        continue
    fi
    reached="$(printf '%s\n' "$tree" | awk -v root="$root/" -v r="$r" '
        {
            i = index($0, "(" root)
            if (!i) next
            rest = substr($0, i + 1 + length(root))
            sub(/\).*/, "", rest)
            split(rest, a, "/")
            gr = a[1] == "nexum" ? 0 : a[1] == "videre" ? 1 : a[1] == "shepherd" ? 2 : -1
            if (gr > r) print rest
        }' | sort -u)"
    if [[ -n $reached ]]; then
        fail "$m reaches down-layer crates: $(tr '\n' ' ' <<<"$reached")"
        dag_bad=1
    fi
done
[[ $dag_bad -eq 0 ]] && pass "crate DAG acyclic across the groupings (${#members[@]} members)"

# 2. Path deps: resolve every member path dep; a downward edge breaks
#    the DAG, and a cross-group edge into a patched grouping must
#    already be a git pin (collected here, judged in section 3).
declare -A cross_path_into=()
path_bad=0
for m in "${members[@]}"; do
    g="${m%%/*}"
    r="$(rank "$g")"
    while IFS= read -r p; do
        [[ -z $p ]] && continue
        rel="$(realpath -m "$root/$m/$p")"
        rel="${rel#"$root"/}"
        tg="${rel%%/*}"
        trank="$(rank "$tg")"
        if [[ $trank -lt 0 ]]; then
            fail "$m/Cargo.toml path dep escapes the workspace: $p"
            path_bad=1
        elif [[ $trank -gt $r ]]; then
            fail "$m/Cargo.toml path dep points down-layer: $p"
            path_bad=1
        elif [[ $tg != "$g" ]]; then
            cross_path_into[$tg]+="$m/Cargo.toml -> $rel "
        fi
    done < <(path_dep_values "$m/Cargo.toml")
done
[[ $path_bad -eq 0 ]] && pass "no downward path dep"

# 3. Cross-group Rust pins: git-tag pins on the owning repos, one tag
#    per repo, each neutralised by an umbrella [patch] entry targeting
#    the owning grouping's in-tree crate, with no orphan entry and no
#    leftover cross-group path dep once the patch exists.
# Live manifests are read by the umbrella build and need [patch]
# neutralisation; Cargo.repo.toml is inert until the carve and only
# joins the tag-agreement check.
manifest_files=(Cargo.toml)
for m in "${members[@]}"; do manifest_files+=("$m/Cargo.toml"); done
for g in "${layers[@]}"; do
    [[ -f $g/Cargo.repo.toml ]] && manifest_files+=("$g/Cargo.repo.toml")
done

declare -A tag_for=()
for slug in nexum-runtime videre-nexum-module; do
    url="https://github.com/nullislabs/$slug"
    owner="$(slug_owner "$slug")"
    pins="$(rg -Hn --no-heading "git *= *\"${url}(\\.git)?\"" "${manifest_files[@]}" || true)"
    patch_body="$(awk -v hdr="[patch.\"$url\"]" '
        index($0, hdr) == 1 {f=1; next}
        /^\[/{f=0}
        f && NF && $0 !~ /^[[:space:]]*#/' Cargo.toml)"

    if [[ -z $pins && -z $patch_body ]]; then
        skip "$slug pins and umbrella patch not written yet"
        continue
    fi
    slug_ok=1

    pin_crates=""
    tags=""
    live_pins=0
    while IFS= read -r hit; do
        [[ -z $hit ]] && continue
        file="${hit%%:*}"
        loc="$file:$(cut -d: -f2 <<<"$hit")"
        line="${hit#*:}"
        line="${line#*:}"
        if [[ $line =~ tag\ *=\ *\"([^\"]+)\" ]]; then
            tags+="${BASH_REMATCH[1]}"$'\n'
        else
            fail "$loc: $slug git pin without an inline tag"
            slug_ok=0
        fi
        # Inert Cargo.repo.toml pins join the tag agreement only.
        [[ $file == */Cargo.repo.toml ]] && continue
        live_pins=1
        pin_crates+="$(sed -E 's/^[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*=.*/\1/' <<<"$line")"$'\n'
    done <<<"$pins"
    pin_crates="$(sort -u <<<"$pin_crates" | rg -v '^$' || true)"
    uniq_tags="$(sort -u <<<"$tags" | rg -v '^$' || true)"
    if [[ -n $uniq_tags && $(wc -l <<<"$uniq_tags") -gt 1 ]]; then
        fail "$slug pinned at more than one tag: $(tr '\n' ' ' <<<"$uniq_tags")"
        slug_ok=0
    elif [[ -n $uniq_tags ]]; then
        tag_for[$slug]="$uniq_tags"
    fi

    if [[ $live_pins -eq 1 && -z $patch_body ]]; then
        fail "umbrella [patch] section missing for $url"
        continue
    fi
    if [[ $live_pins -eq 0 && -z $patch_body ]]; then
        [[ $slug_ok -eq 1 ]] && pass "$slug pinned at ${tag_for[$slug]:-?} (inert manifests only)"
        continue
    fi

    patch_keys=""
    while IFS= read -r entry; do
        [[ -z $entry ]] && continue
        key="$(sed -E 's/^[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*=.*/\1/' <<<"$entry")"
        p="$(rg -o 'path *= *"([^"]+)"' -r '$1' <<<"$entry" || true)"
        patch_keys+="$key"$'\n'
        if [[ -z $p ]]; then
            fail "umbrella patch entry $key for $slug carries no path"
            slug_ok=0
            continue
        fi
        if [[ ! -f $p/Cargo.toml ]]; then
            fail "umbrella patch $key -> $p: no crate there"
            slug_ok=0
            continue
        fi
        if [[ "${p%%/*}" != "$owner" ]]; then
            fail "umbrella patch $key -> $p leaves the $owner grouping"
            slug_ok=0
        fi
        if [[ "$(crate_name "$p")" != "$key" ]]; then
            fail "umbrella patch $key -> $p names crate $(crate_name "$p")"
            slug_ok=0
        fi
    done <<<"$patch_body"
    patch_keys="$(sort -u <<<"$patch_keys" | rg -v '^$' || true)"

    unpatched="$(comm -23 <(printf '%s\n' "${pin_crates:-}") <(printf '%s\n' "${patch_keys:-}") | rg -v '^$' || true)"
    if [[ -n $unpatched ]]; then
        fail "$slug pins not neutralised by the umbrella patch: $(tr '\n' ' ' <<<"$unpatched")"
        slug_ok=0
    fi
    orphans="$(comm -13 <(printf '%s\n' "${pin_crates:-}") <(printf '%s\n' "${patch_keys:-}") | rg -v '^$' || true)"
    if [[ -n $orphans ]]; then
        fail "umbrella patch entries with no $slug pin: $(tr '\n' ' ' <<<"$orphans")"
        slug_ok=0
    fi

    if [[ -n ${cross_path_into[$owner]:-} ]]; then
        fail "cross-group path deps into $owner/ must be $slug git pins: ${cross_path_into[$owner]}"
        slug_ok=0
    fi
    [[ $slug_ok -eq 1 ]] && pass "$slug pinned at ${tag_for[$slug]:-?} and neutralised by the umbrella patch"
done

# 4. Per-group Cargo.repo.toml: the carve-time workspace root must list
#    exactly the umbrella's members for its grouping, and its path deps
#    must stay inside the grouping.
for g in "${layers[@]}"; do
    f="$g/Cargo.repo.toml"
    if [[ ! -f $f ]]; then
        skip "$f not written yet"
        continue
    fi
    repo_members="$(toml_members "$f" | sort)"
    expect="$(printf '%s\n' "${members[@]}" | rg "^$g/" | sed "s|^$g/||" | sort)"
    if [[ "$repo_members" != "$expect" ]]; then
        fail "$f member list disagrees with the umbrella workspace:"
        diff <(printf '%s\n' "$expect") <(printf '%s\n' "$repo_members") >&2 || true
    else
        pass "$f members mirror the umbrella"
    fi
    while IFS= read -r p; do
        [[ -z $p ]] && continue
        rel="$(realpath -m "$root/$g/$p")"
        [[ $rel == "$root/$g/"* ]] || fail "$f path dep leaves the grouping: $p"
    done < <(path_dep_values "$f")
done

# 5. WIT: cross-group wit/ symlinks never point down-layer; wit-deps
#    manifests and locks exist together, stay key-synced, source only
#    up-layer repos, and their tags agree with the Rust pins and across
#    groups: one tag per upstream repo everywhere.
wit_bad=0
declare -A wit_tag_seen=()
declare -A wit_tag_where=()
for g in "${layers[@]}"; do
    r="$(rank "$g")"
    [[ -d $g/wit ]] || continue
    while IFS= read -r link; do
        rel="$(realpath -m "$link")"
        rel="${rel#"$root"/}"
        trank="$(rank "${rel%%/*}")"
        if [[ $trank -lt 0 || $trank -gt $r ]]; then
            fail "$link symlink points down-layer or outside: $rel"
            wit_bad=1
        fi
    done < <(find "$g/wit" -type l)

    toml="$g/wit/deps.toml"
    lock="$g/wit/deps.lock"
    if [[ ! -f $toml && ! -f $lock ]]; then
        skip "$g wit-deps manifest not written yet"
        continue
    fi
    if [[ ! -f $toml || ! -f $lock ]]; then
        fail "$g wit-deps manifest and lock must exist together"
        wit_bad=1
        continue
    fi
    if [[ $g == nexum ]]; then
        if rg -q '^\s*[^#[:space:]]' "$toml" "$lock"; then
            fail "nexum wit-deps must stay empty; nexum:host is a leaf"
            wit_bad=1
        fi
        continue
    fi
    while IFS= read -r key; do
        [[ -z $key ]] && continue
        if ! rg -q "^(\\[$key\\]|$key *=)" "$lock"; then
            fail "$toml key $key missing from $lock"
            wit_bad=1
        fi
    done < <({ rg -o '^([A-Za-z0-9_-]+) *=' -r '$1' "$toml" || true; rg -o '^\[([A-Za-z0-9_-]+)\]' -r '$1' "$toml" || true; } | sort -u)
    for slug in nexum-runtime videre-nexum-module shepherd; do
        owner="$(slug_owner "$slug")"
        owner_rank="$(rank "${owner:-shepherd}")"
        hits="$(rg --no-filename "nullislabs/$slug" "$toml" "$lock" || true)"
        [[ -z $hits ]] && continue
        if [[ $owner_rank -ge $r ]]; then
            fail "$g wit-deps sources nullislabs/$slug, not an up-layer repo"
            wit_bad=1
            continue
        fi
        wit_tags="$({
            rg -o 'refs/tags/([^/"]+)' -r '$1' <<<"$hits" | sed -E 's/\.(tar\.gz|tgz|zip)$//'
            rg -o 'tag *= *"([^"]+)"' -r '$1' <<<"$hits"
        } | sort -u | rg -v '^$' || true)"
        [[ -z $wit_tags ]] && continue
        if [[ $(wc -l <<<"$wit_tags") -gt 1 ]]; then
            fail "$g wit-deps pins nullislabs/$slug at more than one tag: $(tr '\n' ' ' <<<"$wit_tags")"
            wit_bad=1
        elif [[ -n ${tag_for[$slug]:-} && $wit_tags != "${tag_for[$slug]}" ]]; then
            fail "$g wit-deps tag $wit_tags disagrees with the Rust pin ${tag_for[$slug]} for nullislabs/$slug"
            wit_bad=1
        elif [[ -n ${wit_tag_seen[$slug]:-} && $wit_tags != "${wit_tag_seen[$slug]}" ]]; then
            fail "$g wit-deps tag $wit_tags disagrees with the ${wit_tag_where[$slug]} wit-deps tag ${wit_tag_seen[$slug]} for nullislabs/$slug"
            wit_bad=1
        else
            wit_tag_seen[$slug]="$wit_tags"
            wit_tag_where[$slug]="$g"
        fi
    done
done
[[ $wit_bad -eq 0 ]] && pass "wit artefacts agree where written"

exit "$status"
