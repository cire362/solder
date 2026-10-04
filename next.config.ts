import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  images: {
    // Placeholder photography for blog and about pages. Swap for real assets before launch.
    remotePatterns: [
      { protocol: "https", hostname: "picsum.photos" },
      { protocol: "https", hostname: "fastly.picsum.photos" },
    ],
  },
};

export default nextConfig;
