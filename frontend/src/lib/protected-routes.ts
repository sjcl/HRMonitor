export const PROTECTED_ROUTE_PREFIXES = [
  "/me",
  "/groups",
  "/settings",
  "/users",
] as const;

export function isProtectedPathname(pathname: string) {
  return PROTECTED_ROUTE_PREFIXES.some(
    (prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`),
  );
}
