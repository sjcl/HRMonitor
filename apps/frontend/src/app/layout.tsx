import type { Metadata } from "next";
import "./globals.css";
import { Providers } from "./providers";

export const metadata: Metadata = {
  title: "HR Monitor",
  description: "Heart rate monitoring dashboard",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body className="bg-gray-950 text-gray-100 min-h-screen">
        <Providers>
          <nav className="border-b border-gray-800 px-6 py-4">
            <a href="/users" className="text-xl font-bold">
              HR Monitor
            </a>
          </nav>
          <main className="max-w-5xl mx-auto p-6">{children}</main>
        </Providers>
      </body>
    </html>
  );
}
