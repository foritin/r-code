import { useEffect, useState } from "react";
import { platformCapabilities } from "./ipc";
import type { PlatformCapabilities } from "./types";

const NO_NATIVE_CAPABILITIES: PlatformCapabilities = {
  platform: "other",
  nativeOcr: false,
  nativeOcrFormats: [],
};

let cached: PlatformCapabilities | null = null;
let pending: Promise<PlatformCapabilities> | null = null;

export function loadPlatformCapabilities(): Promise<PlatformCapabilities> {
  if (cached) return Promise.resolve(cached);
  pending ??= platformCapabilities()
    .then((capabilities) => {
      cached = capabilities;
      return capabilities;
    })
    .finally(() => {
      pending = null;
    });
  return pending;
}

/** Fail closed until the native host reports its compiled capabilities. */
export function usePlatformCapabilities(): PlatformCapabilities {
  const [capabilities, setCapabilities] = useState(cached ?? NO_NATIVE_CAPABILITIES);
  useEffect(() => {
    let active = true;
    void loadPlatformCapabilities()
      .then((value) => {
        if (active) setCapabilities(value);
      })
      .catch(() => {
        if (active) setCapabilities(NO_NATIVE_CAPABILITIES);
      });
    return () => {
      active = false;
    };
  }, []);
  return capabilities;
}
