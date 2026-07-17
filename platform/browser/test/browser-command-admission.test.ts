import assert from "node:assert/strict";
import test from "node:test";

import {
  BrowserCommandAdmission,
  BrowserCommandAdmissionStateError,
  MAX_BROWSER_ADMISSION_REQUESTS,
  MAX_BROWSER_ADMISSION_SESSIONS,
  MAX_BROWSER_ADMISSION_SURFACES,
  isBrowserCommandAdmission,
  type BrowserCommandAdmissionLimits,
  type BrowserCommandAdmissionStateErrorCode,
} from "../src/browser-command-admission.js";

const TEST_LIMITS = Object.freeze({
  maxSessions: 4,
  maxRequests: 8,
  maxSurfaces: 8,
});

const assertStateError = (
  operation: () => unknown,
  code: BrowserCommandAdmissionStateErrorCode,
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserCommandAdmissionStateError
      && error.code === code
      && error.message === code,
  );
};

test("rejects invalid admission limits with a stable content-free error", () => {
  for (const limits of [
    { maxSessions: -1, maxRequests: 1, maxSurfaces: 1 },
    { maxSessions: 1, maxRequests: 1.5, maxSurfaces: 1 },
    {
      maxSessions: 1,
      maxRequests: 1,
      maxSurfaces: Number.POSITIVE_INFINITY,
    },
    {
      maxSessions: 1,
      maxRequests: 1,
      maxSurfaces: 1,
      extra: 1,
    },
  ]) {
    assertStateError(
      () =>
        new BrowserCommandAdmission(
          "Ready",
          limits as BrowserCommandAdmissionLimits,
        ),
      "InvalidAdmissionConfiguration",
    );
  }

  const accessorLimits = {
    get maxSessions(): number {
      return 1;
    },
    maxRequests: 1,
    maxSurfaces: 1,
  };
  assertStateError(
    () => new BrowserCommandAdmission("Ready", accessorLimits),
    "InvalidAdmissionConfiguration",
  );
  const nullPrototypeLimits = Object.assign(
    Object.create(null),
    TEST_LIMITS,
  ) as BrowserCommandAdmissionLimits;
  assertStateError(
    () =>
      new BrowserCommandAdmission(
        "Ready",
        nullPrototypeLimits,
      ),
    "InvalidAdmissionConfiguration",
  );
});

test("accepts zero and hard maxima and rejects each hard maximum plus one", () => {
  assert.doesNotThrow(
    () =>
      new BrowserCommandAdmission("Ready", {
        maxSessions: MAX_BROWSER_ADMISSION_SESSIONS,
        maxRequests: MAX_BROWSER_ADMISSION_REQUESTS,
        maxSurfaces: MAX_BROWSER_ADMISSION_SURFACES,
      }),
  );

  const zero = new BrowserCommandAdmission("Ready", {
    maxSessions: 0,
    maxRequests: 0,
    maxSurfaces: 0,
  });
  assertStateError(
    () => zero.setSessionState(2n, "Opening"),
    "AdmissionCapacityExceeded",
  );
  assertStateError(
    () => zero.setRequestState(9n, "Active"),
    "AdmissionCapacityExceeded",
  );

  const zeroSurfaces = new BrowserCommandAdmission("Ready", {
    maxSessions: 1,
    maxRequests: 0,
    maxSurfaces: 0,
  });
  zeroSurfaces.setSessionState(2n, "Ready");
  assertStateError(
    () => zeroSurfaces.setSurfaceState(4n, 2n, "Alive"),
    "AdmissionCapacityExceeded",
  );

  for (const limits of [
    {
      maxSessions: MAX_BROWSER_ADMISSION_SESSIONS + 1,
      maxRequests: MAX_BROWSER_ADMISSION_REQUESTS,
      maxSurfaces: MAX_BROWSER_ADMISSION_SURFACES,
    },
    {
      maxSessions: MAX_BROWSER_ADMISSION_SESSIONS,
      maxRequests: MAX_BROWSER_ADMISSION_REQUESTS + 1,
      maxSurfaces: MAX_BROWSER_ADMISSION_SURFACES,
    },
    {
      maxSessions: MAX_BROWSER_ADMISSION_SESSIONS,
      maxRequests: MAX_BROWSER_ADMISSION_REQUESTS,
      maxSurfaces: MAX_BROWSER_ADMISSION_SURFACES + 1,
    },
  ]) {
    assertStateError(
      () => new BrowserCommandAdmission("Ready", limits),
      "InvalidAdmissionConfiguration",
    );
  }
});

test("enforces exact session, request, and Surface capacities", () => {
  const admission = new BrowserCommandAdmission("Ready", {
    maxSessions: 1,
    maxRequests: 1,
    maxSurfaces: 1,
  });

  admission.setSessionState(2n, "Opening");
  admission.setSessionState(2n, "Ready");
  admission.setActiveGeneration(2n, 1n);
  admission.setActiveGeneration(2n, 1n);
  admission.setActiveGeneration(2n, 2n);
  assertStateError(
    () => admission.setSessionState(3n, "Ready"),
    "AdmissionCapacityExceeded",
  );

  admission.setRequestState(9n, "Active", 2n);
  admission.setRequestState(9n, "Terminal", 2n);
  assertStateError(
    () => admission.setRequestState(10n, "Active", 2n),
    "AdmissionCapacityExceeded",
  );

  admission.setSurfaceState(4n, 2n, "Alive");
  admission.setSurfaceState(4n, 2n, "Reclaimed");
  assertStateError(
    () => admission.setSurfaceState(5n, 2n, "Alive"),
    "AdmissionCapacityExceeded",
  );
});

test("implements the complete irreversible lifecycle transition models", () => {
  const workerStates = [
    "NotStarted",
    "Starting",
    "Ready",
    "Draining",
    "Stopped",
    "Failed",
  ] as const;
  const allowedWorkerTransitions = new Set([
    "NotStarted->NotStarted",
    "NotStarted->Starting",
    "NotStarted->Failed",
    "Starting->Starting",
    "Starting->Ready",
    "Starting->Failed",
    "Ready->Ready",
    "Ready->Draining",
    "Ready->Failed",
    "Draining->Draining",
    "Draining->Stopped",
    "Draining->Failed",
    "Stopped->Stopped",
    "Failed->Failed",
  ]);
  for (const current of workerStates) {
    for (const next of workerStates) {
      const admission = new BrowserCommandAdmission(current, TEST_LIMITS);
      const operation = (): void => admission.setWorkerState(next);
      if (allowedWorkerTransitions.has(`${current}->${next}`)) {
        assert.doesNotThrow(operation);
      } else {
        assertStateError(operation, "InvalidAdmissionTransition");
      }
    }
  }

  const sessionStates = [
    "Opening",
    "Ready",
    "Closing",
    "Closed",
  ] as const;
  const allowedSessionTransitions = new Set([
    "Opening->Opening",
    "Opening->Ready",
    "Opening->Closing",
    "Opening->Closed",
    "Ready->Ready",
    "Ready->Closing",
    "Ready->Closed",
    "Closing->Closing",
    "Closing->Closed",
    "Closed->Closed",
  ]);
  for (const current of sessionStates) {
    for (const next of sessionStates) {
      const admission = new BrowserCommandAdmission("Ready", TEST_LIMITS);
      admission.setSessionState(2n, current);
      const operation = (): void => admission.setSessionState(2n, next);
      if (allowedSessionTransitions.has(`${current}->${next}`)) {
        assert.doesNotThrow(operation);
      } else {
        assertStateError(operation, "InvalidAdmissionTransition");
      }
    }
  }

  const requestStates = ["Active", "Terminal"] as const;
  for (const current of requestStates) {
    for (const next of requestStates) {
      const admission = new BrowserCommandAdmission("Ready", TEST_LIMITS);
      admission.setRequestState(9n, current);
      const operation = (): void => admission.setRequestState(9n, next);
      if (current === "Active" || next === "Terminal") {
        assert.doesNotThrow(operation);
      } else {
        assertStateError(operation, "InvalidAdmissionTransition");
      }
    }
  }

  const surfaceStates = ["Alive", "Reclaimed"] as const;
  for (const current of surfaceStates) {
    for (const next of surfaceStates) {
      const admission = new BrowserCommandAdmission("Ready", TEST_LIMITS);
      admission.setSessionState(2n, "Ready");
      admission.setSurfaceState(4n, 2n, current);
      const operation = (): void =>
        admission.setSurfaceState(4n, 2n, next);
      if (current === "Alive" || next === "Reclaimed") {
        assert.doesNotThrow(operation);
      } else {
        assertStateError(operation, "InvalidAdmissionTransition");
      }
    }
  }
});

test("terminal states, owners, and generation high-watermarks are irreversible", () => {
  const worker = new BrowserCommandAdmission("NotStarted", TEST_LIMITS);
  worker.setWorkerState("NotStarted");
  worker.setWorkerState("Starting");
  worker.setWorkerState("Ready");
  worker.setWorkerState("Draining");
  worker.setWorkerState("Stopped");
  worker.setWorkerState("Stopped");
  assertStateError(
    () => worker.setWorkerState("Ready"),
    "InvalidAdmissionTransition",
  );

  const failedWorker = new BrowserCommandAdmission("Starting", TEST_LIMITS);
  failedWorker.setWorkerState("Failed");
  failedWorker.setWorkerState("Failed");
  assertStateError(
    () => failedWorker.setWorkerState("Starting"),
    "InvalidAdmissionTransition",
  );

  const admission = new BrowserCommandAdmission("Ready", TEST_LIMITS);
  admission.setSessionState(2n, "Opening");
  admission.setSessionState(2n, "Ready");
  admission.setSessionState(2n, "Closing");
  admission.setSessionState(2n, "Closed");
  admission.setSessionState(2n, "Closed");
  assertStateError(
    () => admission.setSessionState(2n, "Opening"),
    "InvalidAdmissionTransition",
  );

  admission.setSessionState(3n, "Ready");
  admission.setSessionState(4n, "Ready");

  admission.setRequestState(9n, "Active", 3n);
  admission.setRequestState(9n, "Active", 3n);
  admission.setRequestState(9n, "Terminal", 3n);
  admission.setRequestState(9n, "Terminal", 3n);
  assertStateError(
    () => admission.setRequestState(9n, "Active", 3n),
    "InvalidAdmissionTransition",
  );
  admission.setRequestState(10n, "Active", 3n);
  assertStateError(
    () => admission.setRequestState(10n, "Active", 4n),
    "InvalidAdmissionTransition",
  );
  admission.setRequestState(11n, "Active");
  assertStateError(
    () => admission.setRequestState(11n, "Active", 3n),
    "InvalidAdmissionTransition",
  );

  admission.setSurfaceState(20n, 3n, "Alive");
  admission.setSurfaceState(20n, 3n, "Alive");
  admission.setSurfaceState(20n, 3n, "Reclaimed");
  admission.setSurfaceState(20n, 3n, "Reclaimed");
  assertStateError(
    () => admission.setSurfaceState(20n, 3n, "Alive"),
    "InvalidAdmissionTransition",
  );
  admission.setSurfaceState(21n, 3n, "Alive");
  assertStateError(
    () => admission.setSurfaceState(21n, 4n, "Alive"),
    "InvalidAdmissionTransition",
  );

  admission.setActiveGeneration(3n, 5n);
  admission.setActiveGeneration(3n, 5n);
  admission.setActiveGeneration(3n, 6n);
  assertStateError(
    () => admission.setActiveGeneration(3n, 5n),
    "InvalidAdmissionTransition",
  );
});

test("rejects subclass and exotic construction and freezes authentic instances", () => {
  const admission = new BrowserCommandAdmission("Ready", TEST_LIMITS);
  assert.ok(Object.isFrozen(admission));
  assert.ok(Object.isFrozen(BrowserCommandAdmission.prototype));
  assert.throws(
    () =>
      Object.defineProperty(admission, "validate", {
        value: (): undefined => undefined,
      }),
    TypeError,
  );

  class DerivedAdmission extends BrowserCommandAdmission {}
  assertStateError(
    () => new DerivedAdmission("Ready", TEST_LIMITS),
    "InvalidAdmissionConfiguration",
  );

  function ExoticAdmission(): void {}
  assertStateError(
    () =>
      Reflect.construct(
        BrowserCommandAdmission,
        ["Ready", TEST_LIMITS],
        ExoticAdmission,
      ),
    "InvalidAdmissionConfiguration",
  );
});

test("admission authenticity ignores WeakSet prototype pollution", () => {
  const originalAdd = WeakSet.prototype.add;
  const originalHas = WeakSet.prototype.has;
  assert.equal(
    Reflect.set(WeakSet.prototype, "add", () => new WeakSet<object>()),
    true,
  );
  assert.equal(
    Reflect.set(WeakSet.prototype, "has", () => true),
    true,
  );
  try {
    assert.equal(
      isBrowserCommandAdmission(
        Object.create(BrowserCommandAdmission.prototype),
      ),
      false,
    );
    assert.equal(
      isBrowserCommandAdmission(
        new BrowserCommandAdmission("Ready", TEST_LIMITS),
      ),
      true,
    );
  } finally {
    const addRestored = Reflect.set(
      WeakSet.prototype,
      "add",
      originalAdd,
    );
    const hasRestored = Reflect.set(
      WeakSet.prototype,
      "has",
      originalHas,
    );
    assert.equal(addRestored, true);
    assert.equal(hasRestored, true);
  }
});
