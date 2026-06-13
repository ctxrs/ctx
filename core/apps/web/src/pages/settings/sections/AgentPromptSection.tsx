import type { ReactNode } from "react";

type SectionProps = {
  children: ReactNode;
};

export function AgentPromptSection({ children }: SectionProps) {
  return <>{children}</>;
}
