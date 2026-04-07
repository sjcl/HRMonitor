import { auth } from "@/lib/auth";
import { isProtectedPathname } from "@/lib/protected-routes";
import { NextResponse } from "next/server";

export const proxy = auth((req) => {
  if (!isProtectedPathname(req.nextUrl.pathname)) {
    return NextResponse.next();
  }

  if (!req.auth) {
    const loginUrl = new URL("/login", req.nextUrl.origin);
    return NextResponse.redirect(loginUrl);
  }
});

export const config = {
  matcher: [
    "/((?!login|api/auth|_next/static|_next/image|favicon.ico).*)",
  ],
};
