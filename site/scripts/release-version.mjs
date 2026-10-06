import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export function releaseVersion(release, expectedTag = "") {
  if (release.isDraft !== false || release.isPrerelease !== false) {
    throw new Error("The website requires a published stable release");
  }
  const match = /^v(\d+\.\d+\.\d+)$/.exec(release.tagName);
  if (!match || (expectedTag && release.tagName !== expectedTag)) {
    throw new Error("The published release tag does not match the requested version");
  }
  const version = match[1];
  const asset = `Daisy-${version}-macos-arm64.dmg`;
  if (!release.assets?.some((entry) => entry.name === asset)) {
    throw new Error(`The release is missing ${asset}`);
  }
  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const release = JSON.parse(readFileSync(process.argv[2], "utf8"));
    console.log(releaseVersion(release, process.argv[3] || ""));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
