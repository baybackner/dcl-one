import { useEffect, useState } from "react";

const MAX_IN_FLIGHT = 4;

/**
 * Reads private files into object URLs, revoked when `urls` changes or on unmount.
 * @param signedFetch the session's signed fetch; requests skip the HTTP cache, four at a time
 * @returns each readable url's object URL; a refused file has no entry
 */
export function useSignedObjectUrls(urls: string[], signedFetch: typeof fetch): ReadonlyMap<string, string> {
  const key = [...new Set(urls)].sort().join("\n");
  const [objectUrls, setObjectUrls] = useState<ReadonlyMap<string, string>>(() => new Map());

  useEffect(() => {
    const wanted = key ? key.split("\n") : [];
    const controller = new AbortController();
    const made: string[] = [];
    let next = 0;
    const read = async (url: string) => {
      const response = await signedFetch(url, { signal: controller.signal, cache: "no-store" });
      if (!response.ok) return;
      const blob = await response.blob();
      if (controller.signal.aborted) return;
      const objectUrl = URL.createObjectURL(blob);
      made.push(objectUrl);
      setObjectUrls((current) => new Map(current).set(url, objectUrl));
    };
    const worker = async () => {
      while (next < wanted.length && !controller.signal.aborted) {
        await read(wanted[next++]).catch(() => undefined);
      }
    };
    for (let i = 0; i < MAX_IN_FLIGHT; i++) void worker();
    return () => {
      controller.abort();
      for (const objectUrl of made) URL.revokeObjectURL(objectUrl);
      setObjectUrls(new Map());
    };
  }, [key, signedFetch]);

  return objectUrls;
}
