export const PROTECTED_ROUTE_PREFIXES = [
  "/dashboard",
  "/settings",
  "/users",
] as const;

export function isProtectedPathname(pathname: string) {
  return PROTECTED_ROUTE_PREFIXES.some(
    (prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`),
  );
}
