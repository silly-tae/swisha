-- Schema swisha expects. Table names are configurable via TABLE_PAYOUTS, TABLE_EVENTS and
-- TABLE_LOGS; the defaults are used below.
--
-- swisha stores only what its payout state machine needs. Business context such as invoices,
-- orders or line items belongs in your own tables, keyed by the same reference.

CREATE TABLE IF NOT EXISTS swisha_payouts (
    -- Your identifier for this payout. Opaque to swisha, sent to Swish as
    -- payerPaymentReference. Also the idempotency key for the double-payout guard.
    reference    VARCHAR(35)   PRIMARY KEY,
    payee_alias  VARCHAR(20)   NOT NULL,
    payee_ssn    VARCHAR(12),
    amount       NUMERIC(10,2) NOT NULL,
    message      VARCHAR(255)  NOT NULL,
    -- payoutInstructionUUID: 32 uppercase hex characters. VARCHAR, not CHAR: CHAR is
    -- blank-padded, so a shorter value reads back with trailing spaces and never compares
    -- equal to what was written.
    swish_ref    VARCHAR(32),
    -- CREATED, PENDING, DEBITED, PAID, DECLINED, ERROR, NEEDS_REVIEW
    status       VARCHAR(20)   NOT NULL,
    attempts     INTEGER       NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);

-- The stall sweep scans by status and age together.
CREATE INDEX IF NOT EXISTS idx_swisha_payouts_stalled
    ON swisha_payouts (status, updated_at);

-- One row per Swish event: submission, callback, retry, resolution.
CREATE TABLE IF NOT EXISTS swisha_events (
    id            BIGSERIAL     PRIMARY KEY,
    reference     VARCHAR(35)   NOT NULL,
    swish_ref     VARCHAR(32),
    event         VARCHAR(50)   NOT NULL,
    status        VARCHAR(20),
    amount        NUMERIC(10,2),
    payee_alias   VARCHAR(20),
    error_code    VARCHAR(20),
    error_message TEXT,
    ip            VARCHAR(45),
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swisha_events_reference  ON swisha_events (reference);
CREATE INDEX IF NOT EXISTS idx_swisha_events_created_at ON swisha_events (created_at);

-- General service log.
CREATE TABLE IF NOT EXISTS swisha_logs (
    id        BIGSERIAL   PRIMARY KEY,
    level     VARCHAR(20) NOT NULL,
    message   TEXT        NOT NULL,
    context   TEXT,
    ip        VARCHAR(45),
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
