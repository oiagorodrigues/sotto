const PATHS = {
  "eye-off": [
    "M3 3l18 18",
    "M10.6 6.1A9.6 9.6 0 0 1 12 6c5 0 8.5 4 9.5 6a14 14 0 0 1-2.7 3.5M6.4 7.7A14 14 0 0 0 2.5 12c1 2 4.5 6 9.5 6 1.4 0 2.7-.3 3.8-.8",
    "M9.9 9.9a3 3 0 0 0 4.2 4.2",
  ],
  eye: [
    "M2.5 12c1-2 4.5-6 9.5-6s8.5 4 9.5 6c-1 2-4.5 6-9.5 6s-8.5-4-9.5-6z",
    "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
  ],
  camera: ["M4 8h3l1.5-2h7L17 8h3v11H4z", "M12 10a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z"],
  corner: ["M20 5v7a3 3 0 0 1-3 3H5", "m9 11-4 4 4 4"],
  copy: ["M8 8h12v12H8z", "M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"],
  bolt: ["M13 3 5 14h6l-1 7 8-11h-6z"],
  key: ["M8 11a4 4 0 1 0 0 8 4 4 0 0 0 0-8z", "m11 12 9-9M16 7l3 3"],
  trash: ["M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"],
  swap: ["M4 8h13l-3-3M20 16H7l3 3"],
  refresh: ["M20 11a8 8 0 0 0-14-4L4 9M4 4v5h5M4 13a8 8 0 0 0 14 4l2-2M20 20v-5h-5"],
  info: ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M12 11v5M12 8h.01"],
  gear: [
    "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
    "M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6 7 7M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4",
  ],
  cpu: [
    "M8 6h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z",
    "M9.5 9.5h5v5h-5z",
    "M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3",
  ],
  mic: ["M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0M12 18v3"],
  layers: ["m12 3 9 5-9 5-9-5z", "m3 13 9 5 9-5"],
  command: [
    "M9 9V6a3 3 0 1 0-3 3h3zm0 0h6m-6 0v6m6-6V6a3 3 0 1 1 3 3h-3zm0 0v6m0 0h3a3 3 0 1 1-3 3v-3zm0 0H9m0 0H6a3 3 0 1 0 3 3v-3z",
  ],
  shield: ["M12 3 5 6v6c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6z"],
  user: ["M12 4a4 4 0 1 0 0 8 4 4 0 0 0 0-8z", "M4 21a8 8 0 0 1 16 0"],
  cloud: ["M7 18a4 4 0 0 1-.5-8A6 6 0 0 1 18 11a3.5 3.5 0 0 1-.5 7z"],
  calendar: ["M4 6h16v14H4z", "M4 10h16M8 3v4M16 3v4"],
  video: ["M3 7h11v10H3z", "m14 11 7-4v10l-7-4"],
  chevron: ["m6 9 6 6 6-6"],
  plus: ["M12 5v14M5 12h14"],
  file: ["M7 3h7l5 5v13H7z", "M14 3v5h5"],
  check: ["m5 12 5 5 9-10"],
  alert: ["M12 3 2.5 20h19z", "M12 10v4M12 17h.01"],
  stop: ["M7 7h10v10H7z"],
  close: ["M6 6l12 12M18 6 6 18"],
  help: [
    "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z",
    "M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.7.4-1 .9-1 1.7M12 17h.01",
  ],
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size }: { name: IconName; size?: number }) {
  return (
    <svg
      className="i"
      viewBox="0 0 24 24"
      width={size}
      height={size}
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}
