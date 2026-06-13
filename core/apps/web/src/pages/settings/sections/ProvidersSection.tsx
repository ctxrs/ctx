import type { ReactNode } from "react";

type SectionProps = {
  children: ReactNode;
};

export function ProvidersSection({ children }: SectionProps) {
  return <>{children}</>;
}
