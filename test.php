<?php
class SyntaxHlTest {
	public function onNewline( NlTk $token ): ?array {
		$this->env->trace( 'tsp', $this->pipelineId,
			static function () use ( $self, $token ) {
				return "(indep=" . ( $self->inIndependentParse ? "yes" : "no " ) .
					";sol=" . ( $self->sol ? "yes" : "no " ) . ') ' .
					PHPUtils::jsonEncode( $token );
			}
		);
		$this->srcOffset = $token->dataParsoid->tsr->end ?? null;
		$this->tokenBuf[] = $token;
		$this->sol = true;
		return [];
	}
}
