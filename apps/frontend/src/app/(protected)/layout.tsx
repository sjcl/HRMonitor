import { auth } from "@/lib/auth";
import { redirect } from "next/navigation";
import { AuthProviders } from "./auth-providers";
import { Navbar } from "@/components/navbar";

export default async function ProtectedLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const session = await auth();
  if (!session) redirect("/login");
  return (
    <AuthProviders session={session}>
      <Navbar />
      <main className="max-w-5xl mx-auto p-6">{children}</main>
    </AuthProviders>
  );
}
