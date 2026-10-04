export const version = "0.3.0";
export const repo = "https://github.com/misfitdev/daisy";
export const releaseAsset = `Daisy-${version}-macos-arm64.dmg`;
/** The DMG itself. The asset name carries the version, so the link pins its tag. */
export const downloadUrl = `${repo}/releases/download/v${version}/${releaseAsset}`;
export const releasesUrl = `${repo}/releases`;

export function href(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, "");
  return `${base}${path}`;
}
