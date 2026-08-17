import { Link, useParams, useSearchParams } from "react-router";
import { UserDetailView } from "../components/user-detail-view";

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function UserDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();

  // `from` only ever drives a back link, but it still goes into an href, so it
  // is validated rather than interpolated as-is.
  const fromGroup = searchParams.get("from");
  const isValidGroup = fromGroup !== null && UUID_RE.test(fromGroup);
  const backHref = isValidGroup ? `/groups/${fromGroup}` : "/groups";
  const backLabel = isValidGroup ? "Back to group" : "Back to groups";

  if (!id) return null;

  return (
    <div>
      <Link to={backHref} className="text-sm text-gray-400 hover:underline">
        &larr; {backLabel}
      </Link>
      <div className="mt-4">
        <UserDetailView userId={id} />
      </div>
    </div>
  );
}
