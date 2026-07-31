export interface AppModalVisibility {
  cache: boolean;
  licenses: boolean;
}

export function resolveAppModalVisibility(
  cachePromptPending: boolean,
  licensesRequested: boolean
): AppModalVisibility {
  if (licensesRequested) return { cache: false, licenses: true };
  return { cache: cachePromptPending, licenses: false };
}
