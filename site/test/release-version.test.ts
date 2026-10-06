import assert from "node:assert/strict";
import test from "node:test";
import { releaseVersion } from "../scripts/release-version.mjs";

function published(tagName = "v0.6.0") {
  return {
    tagName,
    isDraft: false,
    isPrerelease: false,
    assets: [{ name: `Daisy-${tagName.slice(1)}-macos-arm64.dmg` }],
  };
}

test("release dispatch uses its exact published tag", () => {
  assert.equal(releaseVersion(published(), "v0.6.0"), "0.6.0");
  assert.throws(() => releaseVersion(published("v0.5.0"), "v0.6.0"));
});

test("main and manual builds accept the latest stable release", () => {
  assert.equal(releaseVersion(published("v0.5.0")), "0.5.0");
});

test("unpublished and prerelease versions cannot become download links", () => {
  assert.throws(() => releaseVersion({ ...published(), isDraft: true }));
  assert.throws(() => releaseVersion({ ...published(), isPrerelease: true }));
  assert.throws(() => releaseVersion(published("v0.6.0-beta")));
});

test("a missing DMG fails the build instead of publishing a broken download", () => {
  assert.throws(() => releaseVersion({ ...published(), assets: [] }));
  assert.throws(() => releaseVersion({ ...published(), assets: [{ name: "Daisy-0.5.0-macos-arm64.dmg" }] }));
});
