import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "claude-mem-rs",
  description: "Operational dashboard for the claude-mem-rs memory runtime",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
