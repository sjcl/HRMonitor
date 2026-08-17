import { UserDetailView } from "../components/user-detail-view";
import { SubNav } from "../components/sub-nav";
import { useCurrentUser } from "../lib/auth";

export function MePage() {
  const { data: user } = useCurrentUser();

  return (
    <div>
      <SubNav />
      {user ? <UserDetailView userId={user.id} /> : null}
    </div>
  );
}
