"use client";

import { useMemo } from "react";

const timezones = typeof Intl !== "undefined" && Intl.supportedValuesOf
  ? Intl.supportedValuesOf("timeZone")
  : [];

function groupByRegion(tzList: string[]) {
  const groups: Record<string, string[]> = {};
  for (const tz of tzList) {
    const slash = tz.indexOf("/");
    const region = slash > 0 ? tz.slice(0, slash) : "Other";
    (groups[region] ??= []).push(tz);
  }
  return groups;
}

export function TimezoneSelect({
  value,
  onChange,
  className,
}: {
  value: string;
  onChange: (tz: string) => void;
  className?: string;
}) {
  const grouped = useMemo(() => groupByRegion(timezones), []);

  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={`bg-gray-800 border border-gray-700 rounded px-3 py-2 ${className ?? ""}`}
    >
      {Object.entries(grouped).map(([region, tzs]) => (
        <optgroup key={region} label={region}>
          {tzs.map((tz) => (
            <option key={tz} value={tz}>
              {tz.replace(/_/g, " ")}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
}

export function getBrowserTimezone() {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}
