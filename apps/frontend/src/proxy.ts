import { auth } from "@/lib/auth";
import { NextResponse } from "next/server";

export const proxy = auth((req) => {
  if (!req.auth) {
    const loginUrl = new URL("/login", req.nextUrl.origin);
    return NextResponse.redirect(loginUrl);
  }
});

export const config = {
  matcher: [
    "/dashboard/:path*",
    "/settings/:path*",
    "/users/:path*",
  ],
};
