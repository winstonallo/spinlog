#!/usr/bin/env node
// Usage: node scripts/mb-search.js "search query"
// Searches MusicBrainz release groups (albums/singles/EPs) and prints matches.

const query = process.argv[2];
if (!query) {
  console.error("Usage: node scripts/mb-search.js \"search query\"");
  process.exit(1);
}

const url = new URL("https://musicbrainz.org/ws/2/release-group");
url.searchParams.set("query", query);
url.searchParams.set("limit", "10");
url.searchParams.set("fmt", "json");

const res = await fetch(url, {
  headers: { "User-Agent": "musicboxd-dev/0.1 (evaluation)" },
});

if (!res.ok) {
  console.error(`MusicBrainz error: ${res.status} ${res.statusText}`);
  process.exit(1);
}

const data = await res.json();

if (!data["release-groups"]?.length) {
  console.log("No results found.");
  process.exit(0);
}

for (const rg of data["release-groups"]) {
  const artist = rg["artist-credit"]?.map(c => c.artist?.name ?? c.name).join(", ") ?? "Unknown";
  const year = rg["first-release-date"]?.slice(0, 4) ?? "????";
  const type = [rg["primary-type"], ...(rg["secondary-types"] ?? [])].filter(Boolean).join(" + ") || "Unknown";
  const score = rg.score ?? "?";
  console.log(`[${score}%] ${rg.title} — ${artist} (${year}) [${type}]`);
}
