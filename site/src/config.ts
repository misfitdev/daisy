export const version = "0.1.0";
export const repo = "https://github.com/misfitdev/daisy";
export const releaseAsset = `Daisy-${version}-macos-arm64.zip`;
export const downloadUrl = `${repo}/releases/latest`;

export function href(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, "");
  return `${base}${path}`;
}
