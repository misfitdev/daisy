// Mirrors the Status table in README.md; keep the two in step.
export interface Feature {
  title: string;
  detail: string;
  status: "available" | "planned";
}

export const features: Feature[] = [
  {
    title: "No shared iCloud account",
    detail: "Pair Macs directly with a one-time code. No shared Apple ID or Daisy account.",
    status: "available",
  },
  {
    title: "Trust you control",
    detail: "Choose how long each Mac stays trusted, or forget it instantly.",
    status: "available",
  },
  {
    title: "Direct encrypted connection",
    detail: "Paired Macs exchange input over an authenticated, encrypted connection.",
    status: "available",
  },
  {
    title: "Mac-native Multi-Touch gestures",
    detail: "Use Spaces, Mission Control and App Exposé from the trackpad in front of you.",
    status: "available",
  },
  {
    title: "Shake mouse pointer to locate",
    detail: "Shake the mouse or trackpad to reveal the pointer on the Mac with control.",
    status: "available",
  },
  {
    title: "Peer auto-discovery",
    detail: "Daisy finds trusted Macs on your network without an address.",
    status: "planned",
  },
  {
    title: "Automatic session recovery",
    detail: "Reconnect after sleep, wake and network changes.",
    status: "planned",
  },
  {
    title: "Shared clipboard",
    detail: "Copy on one Mac and paste on another.",
    status: "available",
  },
  {
    title: "Dynamic Mac chaining",
    detail: "Arrange a whole desk of Macs on any edge and move through them as one layout.",
    status: "planned",
  },
];
