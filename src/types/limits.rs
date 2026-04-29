use super::error::ParseError;

/// Hard ceiling on model output tokens per turn. Above this, providers reject the request
/// or silently truncate; we want to fail loudly at the boundary instead.
pub const MAX_OUTPUT_TOKENS_CAP: u32 = 200_000;

/// Hard ceiling on conversation turns per `Agent::reply` invocation. Defends against tool
/// loops where the model never converges on a final answer.
pub const MAX_TURNS_CAP: u32 = 64;

/// Output token budget for a single model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaxOutputTokens(u32);

impl MaxOutputTokens {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for MaxOutputTokens {
    type Error = ParseError;

    fn try_from(n: u32) -> Result<Self, Self::Error> {
        if n == 0 {
            return Err(ParseError::OutOfRange {
                field: "max_output_tokens",
                detail: "must be > 0",
            });
        }
        if n > MAX_OUTPUT_TOKENS_CAP {
            return Err(ParseError::OutOfRange {
                field: "max_output_tokens",
                detail: "exceeds ceiling",
            });
        }
        Ok(Self(n))
    }
}

/// Maximum tool/turn iterations per `Agent::reply` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaxTurns(u32);

impl MaxTurns {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for MaxTurns {
    type Error = ParseError;

    fn try_from(n: u32) -> Result<Self, Self::Error> {
        if n == 0 {
            return Err(ParseError::OutOfRange {
                field: "max_turns",
                detail: "must be > 0",
            });
        }
        if n > MAX_TURNS_CAP {
            return Err(ParseError::OutOfRange {
                field: "max_turns",
                detail: "exceeds ceiling",
            });
        }
        Ok(Self(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_overflow() {
        assert!(MaxOutputTokens::try_from(0).is_err());
        assert!(MaxOutputTokens::try_from(MAX_OUTPUT_TOKENS_CAP + 1).is_err());
        assert!(MaxTurns::try_from(0).is_err());
        assert!(MaxTurns::try_from(MAX_TURNS_CAP + 1).is_err());
    }

    #[test]
    fn accepts_in_range() {
        assert_eq!(MaxOutputTokens::try_from(4096).expect("valid").get(), 4096);
        assert_eq!(MaxTurns::try_from(8).expect("valid").get(), 8);
    }
}
