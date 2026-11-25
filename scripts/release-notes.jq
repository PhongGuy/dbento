# Turns release bodies into notes.json entries. Loaded by release-notes.sh with
# `jq -L scripts 'include "release-notes"; ...'`; every body arrives as data, never as code.

# The release.yml category titles and the kind each one shows as; any other title is "other".
def kind:
  {"New features": "features", "Fixes": "fixes", "Usability and accessibility": "usability",
   "Performance": "performance", "Other changes": "other"}[.] // "other";

# One bullet's text to {text, pr?}: the " by @login in .../pull/N" tail becomes pr, then links keep their
# label, autolinks go, inline markup and backslash escapes are dropped, and whitespace collapses.
def strip_item:
  (capture("^(?<t>.*) by @[A-Za-z0-9-]+(\\[bot\\])? in https://\\S+/pull/(?<n>[0-9]+)\\s*$") // {t: ., n: null}) as $m
  | ($m.t
     | gsub("\\[(?<l>[^\\]]*)\\]\\([^)]*\\)"; "\(.l)")
     | gsub("<http[^>]*>"; "")
     | gsub("`|\\*\\*|__"; "")
     | gsub("\\\\(?<c>.)"; "\(.c)")
     | gsub("\\s+"; " ")
     | ltrimstr(" ") | rtrimstr(" ")) as $text
  | {text: $text} + (if $m.n then {pr: ($m.n | tonumber)} else {} end);

def bullet: capture("^[*+-] (?<b>.*)$").b;
def sentinel: . == "- No pull requests were merged for this release.";

# The grouped items between "## What's Changed" and the next "## Downloads"; [] when either is missing.
# Fails when the window has real bullets but none survives stripping, so empty notes never ship.
def groups($lines):
  [$lines | to_entries[] | select(.value | test("^## What.s Changed\\s*$")) | .key][0] as $start
  | ($start // -1) as $s
  | [$lines | to_entries[] | select(.key > $s and (.value | test("^## Downloads\\s*$"))) | .key][0] as $end
  | if $start == null or $end == null then []
    else
      $lines[$start + 1:$end] as $window
      | ($window | map(select(test("^[*+-] ") and (sentinel | not))) | length) as $bullets
      | (reduce $window[] as $line ({groups: [], drop: false};
          if $line | test("^### ") then
            ($line | sub("^### "; "") | sub("\\s+$"; "")) as $title
            | if $title == "New Contributors" then .drop = true
              else .drop = false | .groups += [{kind: ($title | kind), title: $title, items: []}] end
          elif .drop or ($line | sentinel) or ($line | test("^\\*\\*Full Changelog\\*\\*")) then .
          elif $line | test("^[*+-] ") then
            ($line | bullet | strip_item) as $item
            | if $item.text == "" then .
              elif .groups == [] then .groups = [{kind: "other", title: "Other changes", items: [$item]}]
              else .groups[-1].items += [$item] end
          else . end)
        | .groups | map(select(.items != []))) as $groups
      | if $bullets > 0 and $groups == [] then error("release-notes: a What's Changed section has bullets but no items")
        else $groups end
    end;

# One notes.json entry from a release body; the intro is the text before the first "## " heading.
def entry($version; $previous; $date; $body):
  ($body | gsub("\r"; "") | split("\n")) as $lines
  | ([$lines | to_entries[] | select(.value | test("^## ")) | .key][0] // ($lines | length)) as $head
  | ($lines[:$head] | join("\n") | gsub("(?s)<!--.*?-->"; "") | strip_item.text) as $intro
  | {version: $version, previous: $previous, date: $date}
    + (if $intro == "" then {} else {intro: $intro} end)
    + {groups: (try groups($lines) catch error("\(.) (version \($version))"))};

def numeric_version: ltrimstr("v") | split(".") | map(tonumber);

# The listing's published, non-prerelease vX.Y.Z releases older than $tag, newest first.
def published($tag):
  ($tag | try numeric_version catch null) as $top
  | map(select((.draft | not) and (.prerelease | not) and (.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))
          and .tag_name != $tag))
  | map(. + {key: (.tag_name | numeric_version)})
  | map(select($top == null or .key < $top))
  | sort_by(.key) | reverse;

# The whole document: the newest entry from $body, then every earlier published release, each entry's
# previous pointing at the next older one. Input is gh's --slurp listing, an array of pages.
def notes($tag; $date; $body):
  (add // [] | published($tag)) as $older
  | ([{tag_name: $tag, date: $date, body: $body}]
     + ($older | map({tag_name, date: (.published_at // "" | .[0:10]), body: (.body // "")}))) as $all
  | {schema: 1,
     releases: [range(0; $all | length) as $i
       | $all[$i] as $r
       | entry($r.tag_name | ltrimstr("v");
               ($all[$i + 1].tag_name // null | if . == null then null else ltrimstr("v") end);
               $r.date; $r.body)]};
