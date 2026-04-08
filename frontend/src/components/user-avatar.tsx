const sizeClasses = {
  sm: "w-6 h-6 text-xs",
  md: "w-8 h-8 text-sm",
  lg: "w-10 h-10 text-base",
  xl: "w-9 h-9 text-sm",
} as const;

export function UserAvatar({
  src,
  name,
  size = "sm",
}: {
  src: string | null | undefined;
  name: string;
  size?: "sm" | "md" | "lg" | "xl";
}) {
  const cls = sizeClasses[size];

  if (src) {
    return (
      <img
        src={src}
        alt={name}
        className={`${cls} rounded-full object-cover flex-shrink-0`}
      />
    );
  }

  const initial = name.charAt(0).toUpperCase() || "?";
  return (
    <span
      className={`${cls} rounded-full bg-gray-700 flex items-center justify-center flex-shrink-0 font-medium text-gray-300`}
    >
      {initial}
    </span>
  );
}
