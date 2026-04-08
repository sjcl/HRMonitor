import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";

const appDir = path.resolve("src/app/(protected)");
const protectedRoutesFile = path.resolve("src/lib/protected-routes.ts");

function collectPageRoutes(dir, segments = [], routes = new Set()) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const absolutePath = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      collectPageRoutes(absolutePath, [...segments, entry.name], routes);
      continue;
    }

    if (entry.name !== "page.tsx") {
      continue;
    }

    const routeSegments = segments
      .filter((segment) => !segment.startsWith("("))
      .filter((segment) => !segment.startsWith("@"));

    const topLevelSegment = routeSegments[0];
    routes.add(topLevelSegment ? `/${topLevelSegment}` : "/");
  }

  return routes;
}

function extractDeclaredPrefixes(filePath) {
  const source = readFileSync(filePath, "utf8");
  const arrayMatch = source.match(
    /PROTECTED_ROUTE_PREFIXES\s*=\s*\[([\s\S]*?)\]\s*as const/,
  );

  if (!arrayMatch) {
    throw new Error("Could not find PROTECTED_ROUTE_PREFIXES in protected-routes.ts");
  }

  return new Set(
    [...arrayMatch[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]).sort(),
  );
}

const actualRoutes = collectPageRoutes(appDir);
const declaredPrefixes = extractDeclaredPrefixes(protectedRoutesFile);

const missingPrefixes = [...actualRoutes].filter(
  (route) => !declaredPrefixes.has(route),
);
const stalePrefixes = [...declaredPrefixes].filter(
  (route) => !actualRoutes.has(route),
);

if (missingPrefixes.length === 0 && stalePrefixes.length === 0) {
  console.log("Protected route prefixes are in sync.");
  process.exit(0);
}

if (missingPrefixes.length > 0) {
  console.error(
    `Missing protected prefixes: ${missingPrefixes.join(", ")}.`,
  );
}

if (stalePrefixes.length > 0) {
  console.error(
    `Declared prefixes with no matching protected page: ${stalePrefixes.join(", ")}.`,
  );
}

process.exit(1);
