export function shouldMarkEmptySessionSwitchRendered(params: {
  isActive: boolean;
  stateLoaded: boolean;
  listItemCount: number;
}): boolean {
  return params.isActive && params.stateLoaded && params.listItemCount === 0;
}
