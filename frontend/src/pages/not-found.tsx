import { Link } from "react-router";

export function NotFoundPage() {
  return (
    <div className="mx-auto flex min-h-[60vh] max-w-5xl flex-col items-center justify-center gap-4 px-4">
      <h1 className="text-3xl font-bold">404</h1>
      <p className="text-gray-400">ページが見つかりません。</p>
      <Link to="/me" className="text-sm text-blue-400 hover:underline">
        ホームへ戻る
      </Link>
    </div>
  );
}
