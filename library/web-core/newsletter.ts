import { siteHost } from "./site";

export async function subscribeSiteNewsletter(email: string): Promise<void> {
  const response = await fetch("/v1/newsletter", {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify({ email, source: siteHost() }),
  });
  if (!response.ok) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(`Newsletter request returned ${response.status}`);
  }
}
