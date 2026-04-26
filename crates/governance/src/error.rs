use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("insufficient deposit (need 10,000 CALL)")]
    InsufficientDeposit,
    #[error("voting has not started yet")]
    VotingNotStarted,
    #[error("voting period has closed")]
    VotingPeriodClosed,
    #[error("voter has no voting power")]
    NoVotingPower,
    #[error("proposal defeated")]
    ProposalDefeated,
    #[error("proposal not queued for execution")]
    ProposalNotQueued,
    #[error("timelock period has not elapsed")]
    TimelockNotElapsed,
    #[error("voting period has not ended")]
    VotingPeriodNotEnded,
    #[error("execution timeout has not been reached")]
    ExecutionTimeoutNotReached,
    #[error("insufficient balance for delegation")]
    InsufficientBalanceForDelegation,
    #[error("no active delegation")]
    NoDelegation,
    #[error("no validators registered")]
    NoValidators,
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("chain is not paused")]
    NotPaused,
    #[error("voter has already voted on this proposal")]
    AlreadyVoted,
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    #[error("proposal rate limited — {0} blocks remaining before next submission allowed")]
    ProposalRateLimited(u64),
    #[error("unauthorized executor")]
    UnauthorizedExecutor,
}
