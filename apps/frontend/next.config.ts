import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  output: "standalone",
  reactProductionProfiling: process.env.REACT_PROFILING === "true",
};

export default nextConfig;
