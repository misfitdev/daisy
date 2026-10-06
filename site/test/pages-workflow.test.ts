import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const pages = readFileSync(new URL("../../.github/workflows/pages.yml", import.meta.url), "utf8");
const release = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("release publication dispatches Pages outside the tag workflow", () => {
  assert.doesNotMatch(release, /uses:\s*\.\/\.github\/workflows\/pages\.yml/);
  assert.doesNotMatch(pages, /workflow_call:/);
  assert.match(release, /site:\s*\n\s+needs: release/);
  assert.match(release, /DEFAULT_BRANCH: \$\{\{ github\.event\.repository\.default_branch \}\}/);
  assert.match(release, /RELEASE_TAG: \$\{\{ github\.ref_name \}\}/);
  assert.match(release, /gh workflow run pages\.yml[^\n]*--ref "\$DEFAULT_BRANCH"[^\n]*release_tag=\$RELEASE_TAG/);
});

test("Pages checks site PRs without publishing their artifacts", () => {
  assert.match(pages, /pull_request:\s*\n\s+paths:/);
  assert.match(pages, /npm test/);
  assert.match(pages, /deploy:\s*\n\s+if: github\.event_name != 'pull_request'/);
  assert.match(pages, /uses: actions\/upload-pages-artifact@[^\n]+\n\s+if: github\.event_name != 'pull_request'/);
});
