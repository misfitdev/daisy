export const version = "0.1.1";
export const repo = "https://github.com/misfitdev/daisy";
export const releaseAsset = `Daisy-${version}-macos-arm64.zip`;
/** The zip itself. Pinned to this version's tag: `releases/latest` skips
 * pre-releases, and every beta release is one. */
export const downloadUrl = `${repo}/releases/download/v${version}/${releaseAsset}`;
export const releasesUrl = `${repo}/releases`;

export function href(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, "");
  return `${base}${path}`;
}
