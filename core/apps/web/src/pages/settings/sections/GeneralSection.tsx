import type { ReactNode } from "react";

type SectionProps = {
  children: ReactNode;
};

export function GeneralSection({ children }: SectionProps) {
  return <>{children}</>;
}
