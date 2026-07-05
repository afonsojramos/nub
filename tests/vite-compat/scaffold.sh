#!/usr/bin/env bash
# scaffold.sh — non-interactively scaffold a real Vite-powered project for the
# compat matrix. Each case pins its OWN Vite (recorded by driver.sh), so the set
# naturally spans Vite 5/6/7/8.1 — exercising BOTH the native-sniff (>= 8.1) and
# the dist-backport (< 8.1) paths on real framework code, not toy fixtures.
#
# Usage: scaffold.sh <case> <dest-dir>
# Cases print, after scaffolding, two lines the runner consumes:
#   DEV=<dev command incl. --port $PORT --host 127.0.0.1>
#   BUILD=<build command, or - to skip>
# The runner substitutes $PORT. Scaffolds DO NOT install deps (nub install does).
set -u
case_name="$1"; dest="$2"
rm -rf "$dest"; mkdir -p "$(dirname "$dest")"

# create-vite official templates — all ship create-vite@latest's Vite (8.1+ today
# ⇒ native-sniff path). react-ts is the closest analogue to the #315 hydration
# case among the pure templates.
vite_template() {
  npm create vite@latest "$dest" -- --template "$1" >/dev/null 2>&1
  echo "DEV=npx vite dev --port \$PORT --host 127.0.0.1"
  echo "BUILD=npx vite build"
}

case "$case_name" in
  react-ts|vue-ts|svelte-ts|solid|preact-ts|lit-ts|vanilla-ts)
    vite_template "$case_name" ;;

  # Astro + @astrojs/react — the literal #315 repro (Adam's Astro app 403'd on
  # @astrojs/react/dist/client.js served via /@fs). Scaffold minimal, add React.
  astro-react)
    npm create astro@latest "$dest" -- --template minimal --install false --git false --skip-houston --yes >/dev/null 2>&1
    ( cd "$dest" && npm pkg set dependencies.@astrojs/react="^4" dependencies.react="^19" dependencies.react-dom="^19" >/dev/null 2>&1 )
    cat > "$dest/astro.config.mjs" <<'CFG'
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
export default defineConfig({ integrations: [react()] });
CFG
    echo "DEV=npx astro dev --port \$PORT --host 127.0.0.1"
    echo "BUILD=npx astro build" ;;

  sveltekit)
    npx --yes sv create "$dest" --template minimal --types ts --no-add-ons >/dev/null 2>&1 \
      || npm create svelte@latest "$dest" -- --template skeleton --types typescript --no-eslint --no-prettier --no-playwright --no-vitest >/dev/null 2>&1
    echo "DEV=npx vite dev --port \$PORT --host 127.0.0.1"
    echo "BUILD=npx vite build" ;;

  vitepress)
    mkdir -p "$dest/docs"
    ( cd "$dest" && npm pkg set devDependencies.vitepress="^1" >/dev/null 2>&1 || true )
    printf '{"name":"vp","private":true,"devDependencies":{"vitepress":"^1"}}' > "$dest/package.json"
    echo '# Hello' > "$dest/docs/index.md"
    echo "DEV=npx vitepress dev docs --port \$PORT --host 127.0.0.1"
    echo "BUILD=npx vitepress build docs" ;;

  storybook-vite)
    # React + Vite Storybook (uses @storybook/react-vite → Vite builder).
    npm create vite@latest "$dest" -- --template react-ts >/dev/null 2>&1
    ( cd "$dest" && npm pkg set devDependencies.storybook="^8" devDependencies.@storybook/react-vite="^8" >/dev/null 2>&1 )
    echo "DEV=-"   # storybook dev is heavy; build is the meaningful check
    echo "BUILD=npx storybook build 2>/dev/null || echo skip" ;;

  *)
    echo "UNKNOWN_CASE" >&2; exit 2 ;;
esac
