import type { ReactNode } from "react";

type SectionProps = {
  children: ReactNode;
};

export function PrivacySection({ children }: SectionProps) {
  return <>{children}</>;
}
