#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

const defaults = {
  maxProdLines: 350,
  maxProdFiles: 6,
  maxTypes: 10,
  maxRoleNames: 3,
  maxPrimitiveHits: 4,
  maxLifecycleFlags: 2,
  maxHarnessHooks: 0,
  maxCommentRatio: 0.35,
  minCommentLines: 40,
  maxThinAbstractions: 2,
};

const numericOptions = new Map([
  ["--max-prod-lines", "maxProdLines"],
  ["--max-prod-files", "maxProdFiles"],
  ["--max-types", "maxTypes"],
  ["--max-role-names", "maxRoleNames"],
  ["--max-primitive-hits", "maxPrimitiveHits"],
  ["--max-lifecycle-flags", "maxLifecycleFlags"],
  ["--max-harness-hooks", "maxHarnessHooks"],
  ["--max-comment-ratio", "maxCommentRatio"],
  ["--min-comment-lines", "minCommentLines"],
  ["--max-thin-abstractions", "maxThinAbstractions"],
]);

function usage() {
  return `Usage:
  rust-complexity-smells.mjs [--base <git-ref>] [threshold options]
  rust-complexity-smells.mjs --diff-file <path> [threshold options]

Inputs:
  --base <git-ref>               Diff the ref against the current worktree (default: origin/main)
  --diff-file <path>             Read a unified diff, useful for fixtures or saved reviews

Thresholds (warnings appear only when the value is exceeded):
  --max-prod-lines N             ${defaults.maxProdLines}
  --max-prod-files N             ${defaults.maxProdFiles}
  --max-types N                  ${defaults.maxTypes}
  --max-role-names N             ${defaults.maxRoleNames}
  --max-primitive-hits N         ${defaults.maxPrimitiveHits}
  --max-lifecycle-flags N        ${defaults.maxLifecycleFlags}
  --max-harness-hooks N          ${defaults.maxHarnessHooks}
  --max-comment-ratio N          ${defaults.maxCommentRatio}
  --min-comment-lines N          ${defaults.minCommentLines}
  --max-thin-abstractions N      ${defaults.maxThinAbstractions}

This advisory reviews added Rust diff shape. It does not detect authorship, prove
a defect, or act as a CI gate. Warnings exit successfully; usage/runtime errors fail.`;
}

function fail(message) {
  process.stderr.write(`${message}\n\n${usage()}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const config = { ...defaults };
  let base = "origin/main";
  let diffFile;

  for (let i = 0; i < argv.length; i += 1) {
    const option = argv[i];
    if (option === "--help" || option === "-h") {
      process.stdout.write(`${usage()}\n`);
      process.exit(0);
    }
    if (option === "--base" || option === "--diff-file") {
      const value = argv[++i];
      if (!value) fail(`${option} requires a value.`);
      if (option === "--base") base = value;
      else diffFile = value;
      continue;
    }
    const key = numericOptions.get(option);
    if (!key) fail(`Unknown option: ${option}`);
    const raw = argv[++i];
    const value = Number(raw);
    if (raw === undefined || !Number.isFinite(value) || value < 0) {
      fail(`${option} requires a non-negative number.`);
    }
    if (key !== "maxCommentRatio" && !Number.isInteger(value)) {
      fail(`${option} requires an integer.`);
    }
    config[key] = value;
  }

  if (diffFile && argv.includes("--base")) fail("Choose either --base or --diff-file, not both.");
  return { base, config, diffFile };
}

function loadDiff({ base, diffFile }) {
  if (diffFile) {
    try {
      return readFileSync(diffFile, "utf8");
    } catch (error) {
      fail(`Could not read diff file ${diffFile}: ${error.message}`);
    }
  }

  const result = spawnSync("git", ["diff", "--no-ext-diff", "--unified=0", base, "--", "*.rs"], {
    encoding: "utf8",
  });
  if (result.error) fail(`Could not run git diff: ${result.error.message}`);
  if (result.status !== 0) fail(result.stderr.trim() || `git diff exited with ${result.status}.`);
  return result.stdout;
}

function addedRustLines(diff) {
  const lines = [];
  let path;
  for (const raw of diff.split(/\r?\n/)) {
    if (raw.startsWith("diff --git ")) path = undefined;
    if (raw.startsWith("+++ ")) {
      const candidate = raw.slice(4).trim();
      path = candidate === "/dev/null" ? undefined : candidate.replace(/^b\//, "");
      continue;
    }
    if (path?.endsWith(".rs") && raw.startsWith("+") && !raw.startsWith("+++")) {
      lines.push({ path, text: raw.slice(1) });
    }
  }
  return lines;
}

function isProduction(path) {
  return !/(^|\/)(tests?|benches|examples|fixtures?|snapshots?|testdata)(\/|$)/.test(path) &&
    !/(^|\/)(?:test_[^/]+|[^/]+_test)\.rs$/.test(path);
}

function matches(lines, regex) {
  return lines.filter(({ text }) => regex.test(text));
}

function namesFrom(lines, regex) {
  return [...new Set(lines.flatMap(({ text }) => {
    const match = text.match(regex);
    return match ? [match[1]] : [];
  }))].sort();
}

function analyze(diff, config) {
  const added = addedRustLines(diff);
  const production = added.filter(({ path }) => isProduction(path));
  const nonblank = production.filter(({ text }) => text.trim());
  const files = [...new Set(nonblank.map(({ path }) => path))].sort();
  const warnings = [];
  const add = (id, fact, prompt) => warnings.push({ id, fact, prompt });

  if (nonblank.length > config.maxProdLines) {
    add("scale-lines", `${nonblank.length} added nonblank production lines exceed ${config.maxProdLines}.`, "Confirm that the current slice needs this much production code; identify a layer or future slice to delete or defer.");
  }
  if (files.length > config.maxProdFiles) {
    add("scale-files", `${files.length} production Rust files exceed ${config.maxProdFiles}.`, "Check whether the requirement truly crosses this many file boundaries or whether ownership can stay cohesive.");
  }

  const typePattern = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait)\s+([A-Z][A-Za-z0-9_]*)/;
  const typeNames = namesFrom(production, typePattern);
  if (typeNames.length > config.maxTypes) {
    add("types", `${typeNames.length} added type definitions exceed ${config.maxTypes}: ${typeNames.join(", ")}.`, "Remove ceremonial types and keep only states and abstractions required by approved invariants.");
  }

  const declaredNames = namesFrom(production, /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type|fn)\s+([A-Za-z_][A-Za-z0-9_]*)/);
  const roleNames = declaredNames.filter((name) => /(?:Manager|Coordinator|Supervisor|Registry|Context)$/.test(name));
  if (roleNames.length > config.maxRoleNames) {
    add("role-cluster", `${roleNames.length} role-layer names exceed ${config.maxRoleNames}: ${roleNames.join(", ")}.`, "Ask whether one owner and direct calls can replace manager/coordinator layers.");
  }

  const primitiveGroups = [
    ["shared synchronization", /\b(?:Arc|Mutex|RwLock)\b/],
    ["channels or tasks", /\b(?:mpsc|channel|Sender|Receiver|spawn_blocking)\b|(?:thread|tokio)::spawn/],
    ["global state", /\b(?:OnceLock|LazyLock|static\s+mut)\b|^\s*static\s+[A-Z_]+/],
    ["process control", /\b(?:Command::new|std::process::Command|Child|kill\s*\(|wait\s*\()/],
    ["callbacks", /\bBox\s*<\s*dyn\s+Fn/],
    ["unsafe", /\bunsafe\b/],
  ];
  const primitiveHits = primitiveGroups.map(([name, pattern]) => [name, matches(production, pattern).length]).filter(([, count]) => count > 0);
  const primitiveTotal = primitiveHits.reduce((sum, [, count]) => sum + count, 0);
  if (primitiveTotal > config.maxPrimitiveHits) {
    add("primitives", `${primitiveTotal} concurrency/global/process-control hits exceed ${config.maxPrimitiveHits}: ${primitiveHits.map(([name, count]) => `${name}=${count}`).join(", ")}.`, "Make each primitive justify its lifecycle and failure modes; prefer ownership or an existing platform primitive where possible.");
  }

  const lifecyclePattern = /\b(?:is_)?(?:running|started|stopped|ready|active|closed|closing|shutdown|shutting_down|terminated|terminating|exited|healthy)\s*:\s*(?:AtomicBool|bool)\b/;
  const lifecycleFlags = matches(production, lifecyclePattern);
  const stateEnums = namesFrom(production, /^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z0-9_]*(?:State|Status|Lifecycle))\b/);
  if (lifecycleFlags.length > config.maxLifecycleFlags || (stateEnums.length > 0 && lifecycleFlags.length >= 2)) {
    add("lifecycle", `${lifecycleFlags.length} lifecycle booleans${stateEnums.length ? ` overlap with ${stateEnums.join(", ")}` : ""}.`, "Collapse overlapping lifecycle representations or document why each source of truth is independent.");
  }

  const harnessPattern = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:fn|struct|enum|type|const|static)\s+([A-Za-z0-9_]*(?:test|fixture|harness|mock|fault|inject|override|hook)[A-Za-z0-9_]*)/i;
  const harnessNames = [];
  for (let i = 0; i < production.length; i += 1) {
    const match = production[i].text.match(harnessPattern);
    if (!match) continue;
    const nearby = production.slice(Math.max(0, i - 3), i).some(({ path, text }) => path === production[i].path && /#\s*\[cfg[^\]]*test/.test(text));
    if (!nearby) harnessNames.push(match[1]);
  }
  const uniqueHarnessNames = [...new Set(harnessNames)].sort();
  if (uniqueHarnessNames.length > config.maxHarnessHooks) {
    add("harness-hooks", `${uniqueHarnessNames.length} production-visible harness-like declarations exceed ${config.maxHarnessHooks}: ${uniqueHarnessNames.join(", ")}.`, "Verify that production needs each hook; keep test control in test-only code when possible.");
  }

  const commentLines = matches(nonblank, /^\s*(?:\/\/|\/\*|\*)/).length;
  const commentRatio = nonblank.length ? commentLines / nonblank.length : 0;
  if (nonblank.length >= config.minCommentLines && commentRatio > config.maxCommentRatio) {
    add("prose-density", `${commentLines}/${nonblank.length} added production lines are comments (${commentRatio.toFixed(2)}), above ${config.maxCommentRatio}.`, "Keep comments for invariants and design constraints; remove narration that restates the code.");
  }

  const traitNames = namesFrom(production, /^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Z][A-Za-z0-9_]*)/);
  const thinTraits = traitNames.filter((name) => {
    const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const implPattern = new RegExp(`\\bimpl(?:\\s*<[^>]+>)?\\s+${escaped}\\s+for\\b`);
    return matches(production, implPattern).length <= 1;
  });
  const abstractionNames = declaredNames.filter((name) => /(?:Builder|Factory|Provider|Strategy|Adapter|Service|Backend|Facade)$/.test(name));
  const thinAbstractions = [...new Set([...thinTraits, ...abstractionNames])].sort();
  if (thinAbstractions.length > config.maxThinAbstractions) {
    add("abstractions", `${thinAbstractions.length} potentially thin abstractions exceed ${config.maxThinAbstractions}: ${thinAbstractions.join(", ")}.`, "Confirm that current requirements need each indirection; replace one-path builders and one-implementation traits with direct construction where possible.");
  }

  return { added, files, warnings };
}

const options = parseArgs(process.argv.slice(2));
const result = analyze(loadDiff(options), options.config);

process.stdout.write("Advisory Rust diff-shape review; this does not detect authorship, prove defects, or gate CI.\n");
if (!result.warnings.length) {
  process.stdout.write(`No configured thresholds crossed in ${result.files.length} production Rust file(s).\n`);
} else {
  for (const warning of result.warnings) {
    process.stdout.write(`\n[${warning.id}] ${warning.fact}\nReview prompt: ${warning.prompt}\n`);
  }
  process.stdout.write(`\n${result.warnings.length} advisory prompt(s); exit status remains successful.\n`);
}
