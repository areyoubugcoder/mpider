import { useQuery } from "@tanstack/react-query";
import bundledConfig from "../../../config/advertisements.json";
import { ADVERTISEMENTS_REFRESH_MS, loadAdvertisementConfig, readAdvertisementCache, type AdvertisementConfig } from "./advertisements";

export function advertisementStorage() {
  try { return window.localStorage; } catch { return undefined; }
}

export function useAdvertisements() {
  return useQuery({
    queryKey: ["advertisements"],
    queryFn: () => loadAdvertisementConfig({ fallback: bundledConfig as AdvertisementConfig, storage: advertisementStorage(),
      url: import.meta.env.DEV ? import.meta.env.VITE_ADVERTISEMENTS_URL : undefined }),
    initialData: () => readAdvertisementCache(advertisementStorage()) || bundledConfig as AdvertisementConfig,
    initialDataUpdatedAt: 0,
    staleTime: ADVERTISEMENTS_REFRESH_MS,
    refetchInterval: ADVERTISEMENTS_REFRESH_MS,
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: false,
    retry: false,
  }).data;
}
