import { Link } from "react-router";
import { UserMenu } from "@/components/user-menu";
import type { SelfUser } from "@/lib/api";

export function Navbar({ user }: { user: SelfUser }) {
  return (
    <nav className="flex items-center justify-between border-b border-gray-800 px-6 py-4">
      <div className="flex items-center gap-6">
        <Link to="/me" className="text-xl font-bold">
          HR Monitor
        </Link>
      </div>
      <UserMenu user={user} />
    </nav>
  );
}
