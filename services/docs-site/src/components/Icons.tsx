import React from 'react';

void React;

interface IconProps {
  className?: string;
}

export function SearchIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <circle cx="11" cy="11" r="8" stroke="currentColor" strokeLinecap="round" strokeWidth="2" />
      <path d="m21 21-4.3-4.3" stroke="currentColor" strokeLinecap="round" strokeWidth="2" />
    </svg>
  );
}

export function CopyIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 18 18"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path
        d="M14.25 5.25H7.25C6.14543 5.25 5.25 6.14543 5.25 7.25V14.25C5.25 15.3546 6.14543 16.25 7.25 16.25H14.25C15.3546 16.25 16.25 15.3546 16.25 14.25V7.25C16.25 6.14543 15.3546 5.25 14.25 5.25Z"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
      <path
        d="M2.80103 11.998L1.77203 5.07397C1.61003 3.98097 2.36403 2.96397 3.45603 2.80197L10.38 1.77297C11.313 1.63397 12.19 2.16297 12.528 3.00097"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

export function ChevronIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 3 6"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path
        d="M3 0L0 3L3 6"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

export function ChevronDownIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 8 5"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path
        d="M1 1 4 4 7 1"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.15"
      />
    </svg>
  );
}

export function MenuIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 448 512"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M0 96C0 78.3 14.3 64 32 64H416c17.7 0 32 14.3 32 32s-14.3 32-32 32H32C14.3 128 0 113.7 0 96zM0 256c0-17.7 14.3-32 32-32H416c17.7 0 32 14.3 32 32s-14.3 32-32 32H32c-17.7 0-32-14.3-32-32zM448 416c0 17.7-14.3 32-32 32H32c-17.7 0-32-14.3-32-32s14.3-32 32-32H416c17.7 0 32 14.3 32 32z" />
    </svg>
  );
}

export function EllipsisVerticalIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <circle cx="12" cy="5" fill="currentColor" r="1.8" />
      <circle cx="12" cy="12" fill="currentColor" r="1.8" />
      <circle cx="12" cy="19" fill="currentColor" r="1.8" />
    </svg>
  );
}

export function TocIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="none"
      viewBox="0 0 16 16"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M2.44434 12.6665H13.5554" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
      <path d="M2.44434 3.3335H13.5554" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
      <path d="M2.44434 8H7.33323" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" />
    </svg>
  );
}

export function GitHubIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 16 16"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M8 0C3.58 0 0 3.67 0 8.2c0 3.63 2.29 6.7 5.47 7.78.4.08.55-.18.55-.39 0-.19-.01-.83-.01-1.5-2.01.38-2.53-.5-2.69-.96-.09-.24-.48-.96-.82-1.15-.28-.16-.68-.56-.01-.57.63-.01 1.08.59 1.23.84.72 1.24 1.87.89 2.33.68.07-.54.28-.89.5-1.1-1.78-.21-3.64-.92-3.64-4.07 0-.9.31-1.64.82-2.22-.08-.21-.36-1.06.08-2.2 0 0 .67-.22 2.2.85a7.37 7.37 0 0 1 4 0c1.53-1.07 2.2-.85 2.2-.85.44 1.14.16 1.99.08 2.2.51.58.82 1.31.82 2.22 0 3.16-1.87 3.86-3.65 4.07.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.47.55.39A8.22 8.22 0 0 0 16 8.2C16 3.67 12.42 0 8 0Z" />
    </svg>
  );
}

export function XIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z" />
    </svg>
  );
}

export function DiscordIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 16 16"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M13.55 3.06a13.24 13.24 0 0 0-3.28-1.03.05.05 0 0 0-.05.02c-.14.25-.3.58-.41.84a12.18 12.18 0 0 0-3.64 0 8.3 8.3 0 0 0-.42-.84.05.05 0 0 0-.05-.02c-1.14.2-2.24.55-3.28 1.03a.04.04 0 0 0-.02.02C.32 6.15-.26 9.14.03 12.1a.06.06 0 0 0 .02.04 13.3 13.3 0 0 0 4.02 2.05.05.05 0 0 0 .06-.02c.31-.42.58-.87.8-1.34a.05.05 0 0 0-.03-.07 8.63 8.63 0 0 1-1.23-.6.05.05 0 0 1-.01-.08c.08-.06.16-.13.24-.2a.05.05 0 0 1 .05-.01c2.58 1.22 5.38 1.22 7.94 0a.05.05 0 0 1 .05 0c.08.07.16.14.24.2a.05.05 0 0 1-.01.08c-.39.24-.8.44-1.23.6a.05.05 0 0 0-.03.07c.22.47.5.92.81 1.34a.05.05 0 0 0 .06.02 13.26 13.26 0 0 0 4.02-2.05.05.05 0 0 0 .02-.04c.35-3.43-.58-6.4-2.36-9.01a.04.04 0 0 0-.02-.03ZM5.35 10.31c-.78 0-1.42-.73-1.42-1.63s.63-1.64 1.42-1.64c.8 0 1.43.73 1.42 1.64 0 .9-.63 1.63-1.42 1.63Zm5.25 0c-.78 0-1.42-.73-1.42-1.63s.63-1.64 1.42-1.64c.8 0 1.43.73 1.42 1.64 0 .9-.63 1.63-1.42 1.63Z" />
    </svg>
  );
}

export function SlackIcon({ className }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      fill="currentColor"
      viewBox="0 0 54 54"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M11.379 33.9993C11.379 37.1358 8.84512 39.6507 5.7276 39.6507C2.61008 39.6507 0.0572205 37.1168 0.0572205 33.9993C0.0572205 30.8817 2.5911 28.3479 5.70862 28.3479H11.36V33.9993H11.379Z" />
      <path d="M14.1962 33.9997C14.1962 30.8632 16.7301 28.3483 19.8476 28.3483C22.9651 28.3483 25.499 30.8822 25.499 33.9997V48.1353C25.499 51.2718 22.9651 53.7867 19.8476 53.7867C16.7301 53.7867 14.1962 51.2718 14.1962 48.1353V33.9997Z" />
      <path d="M19.8662 11.2673C16.7296 11.2673 14.2148 8.73347 14.2148 5.61594C14.2148 2.49842 16.7486 -0.0354538 19.8662 -0.0354538C22.9837 -0.0354538 25.5175 2.49842 25.5175 5.61594V11.2673H19.8662Z" />
      <path d="M19.8682 14.1334C23.0047 14.1334 25.5196 16.6673 25.5196 19.7848C25.5196 22.9023 22.9857 25.4362 19.8682 25.4362H5.67566C2.53916 25.4362 0.0242615 22.9023 0.0242615 19.7848C0.0242615 16.6673 2.55814 14.1334 5.67566 14.1334H19.8682Z" />
      <path d="M42.5323 19.7853C42.5323 16.6488 45.0662 14.1339 48.1837 14.1339C51.3012 14.1339 53.8351 16.6678 53.8351 19.7853C53.8351 22.9028 51.3012 25.4367 48.1837 25.4367H42.5323V19.7853Z" />
      <path d="M39.7126 19.7934C39.7126 22.9299 37.1787 25.4448 34.0612 25.4448C30.9436 25.4448 28.4098 22.911 28.4098 19.7934V5.61986C28.4098 2.48336 30.9436 -0.0315399 34.0612 -0.0315399C37.1787 -0.0315399 39.7126 2.48336 39.7126 5.61986V19.7934Z" />
      <path d="M34.0376 42.482C37.1741 42.482 39.689 45.0158 39.689 48.1334C39.689 51.2509 37.1552 53.7848 34.0376 53.7848C30.9201 53.7848 28.3862 51.2509 28.3862 48.1334V42.482H34.0376Z" />
      <path d="M34.0381 39.6507C30.9016 39.6507 28.3867 37.1168 28.3867 33.9993C28.3867 30.8818 30.9206 28.3479 34.0381 28.3479H48.2306C51.3671 28.3479 53.882 30.8818 53.882 33.9993C53.882 37.1168 51.3482 39.6507 48.2306 39.6507H34.0381Z" />
    </svg>
  );
}
