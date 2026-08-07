"""SQLite persistence for one-off Discord surveys."""

from __future__ import annotations

import time
from collections.abc import Sequence

from domain.models.survey import (
    MAX_SURVEY_QUESTIONS,
    Survey,
    SurveyDeliveryStatus,
    SurveyDraftAnswer,
    SurveyNotFoundError,
    SurveyQuestion,
    SurveyQuestionResult,
    SurveyQuestionType,
    SurveyRecipient,
    SurveyResponseError,
    SurveyResults,
    SurveySession,
    SurveyStateError,
    SurveyStatus,
    SurveyTargetType,
)
from repositories.base_repository import BaseRepository
from repositories.interfaces import ISurveyRepository


class SurveyRepository(BaseRepository, ISurveyRepository):
    """Store survey lifecycle, delivery claims, and respondent-local drafts."""

    @staticmethod
    def _now() -> int:
        return int(time.time())

    @staticmethod
    def _survey_from_row(row) -> Survey:
        target_type = row["target_type"]
        return Survey(
            survey_id=int(row["survey_id"]),
            guild_id=int(row["guild_id"]),
            title=str(row["title"]),
            status=SurveyStatus(row["status"]),
            target_type=SurveyTargetType(target_type) if target_type is not None else None,
            registered_only=bool(row["registered_only"]),
            created_at=int(row["created_at"]),
            opened_at=int(row["opened_at"]) if row["opened_at"] is not None else None,
            closed_at=int(row["closed_at"]) if row["closed_at"] is not None else None,
        )

    @staticmethod
    def _question_from_row(row) -> SurveyQuestion:
        return SurveyQuestion(
            question_id=int(row["question_id"]),
            survey_id=int(row["survey_id"]),
            position=int(row["position"]),
            prompt=str(row["prompt"]),
            question_type=SurveyQuestionType(row["question_type"]),
            required=bool(row["required"]),
            created_at=int(row["created_at"]),
        )

    @staticmethod
    def _recipient_from_row(row) -> SurveyRecipient:
        return SurveyRecipient(
            recipient_id=int(row["recipient_id"]),
            survey_id=int(row["survey_id"]),
            guild_id=int(row["guild_id"]),
            discord_id=int(row["discord_id"]),
            delivery_status=SurveyDeliveryStatus(row["delivery_status"]),
            attempt_count=int(row["attempt_count"]),
            claimed_at=int(row["claimed_at"]) if row["claimed_at"] is not None else None,
            dm_channel_id=(
                int(row["dm_channel_id"]) if row["dm_channel_id"] is not None else None
            ),
            dm_message_id=(
                int(row["dm_message_id"]) if row["dm_message_id"] is not None else None
            ),
            ui_channel_id=(
                int(row["ui_channel_id"]) if row["ui_channel_id"] is not None else None
            ),
            ui_message_id=(
                int(row["ui_message_id"]) if row["ui_message_id"] is not None else None
            ),
            current_question_id=(
                int(row["current_question_id"])
                if row["current_question_id"] is not None
                else None
            ),
            in_review=bool(row["in_review"]),
            started_at=int(row["started_at"]) if row["started_at"] is not None else None,
            submitted_at=(
                int(row["submitted_at"]) if row["submitted_at"] is not None else None
            ),
            last_error=str(row["last_error"]) if row["last_error"] is not None else None,
            created_at=int(row["created_at"]),
            updated_at=int(row["updated_at"]),
        )

    @staticmethod
    def _answer_from_row(row) -> SurveyDraftAnswer:
        return SurveyDraftAnswer(
            question_id=int(row["question_id"]),
            numeric_value=(
                int(row["numeric_value"]) if row["numeric_value"] is not None else None
            ),
            text_value=str(row["text_value"]) if row["text_value"] is not None else None,
            skipped=bool(row["skipped"]),
            updated_at=int(row["updated_at"]),
        )

    def _require_survey_row(self, cursor, guild_id: int, survey_id: int):
        row = cursor.execute(
            "SELECT * FROM surveys WHERE guild_id = ? AND survey_id = ?",
            (guild_id, survey_id),
        ).fetchone()
        if row is None:
            raise SurveyNotFoundError("Survey not found in this server")
        return row

    def _require_draft_row(self, cursor, guild_id: int, survey_id: int):
        row = self._require_survey_row(cursor, guild_id, survey_id)
        if row["status"] != SurveyStatus.DRAFT.value:
            raise SurveyStateError("Survey questions are immutable after the survey is sent")
        return row

    def _get_questions_with_cursor(self, cursor, guild_id: int, survey_id: int) -> list:
        return cursor.execute(
            """
            SELECT * FROM survey_questions
            WHERE guild_id = ? AND survey_id = ?
            ORDER BY position, question_id
            """,
            (guild_id, survey_id),
        ).fetchall()

    def _get_recipient_by_id(self, cursor, recipient_id: int):
        return cursor.execute(
            "SELECT * FROM survey_recipients WHERE recipient_id = ?",
            (recipient_id,),
        ).fetchone()

    def create_survey(self, guild_id: int, title: str) -> Survey:
        normalized = self.normalize_guild_id(guild_id)
        now = self._now()
        with self.connection() as conn:
            cursor = conn.execute(
                """
                INSERT INTO surveys (guild_id, title, status, created_at)
                VALUES (?, ?, 'draft', ?)
                """,
                (normalized, title, now),
            )
            survey_id = int(cursor.lastrowid)
            row = conn.execute(
                "SELECT * FROM surveys WHERE survey_id = ?",
                (survey_id,),
            ).fetchone()
        return self._survey_from_row(row)

    def list_surveys(self, guild_id: int, status: str | None = None) -> list[Survey]:
        normalized = self.normalize_guild_id(guild_id)
        sql = "SELECT * FROM surveys WHERE guild_id = ?"
        params: list[object] = [normalized]
        if status is not None:
            sql += " AND status = ?"
            params.append(status)
        sql += " ORDER BY survey_id DESC"
        with self.connection() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [self._survey_from_row(row) for row in rows]

    def get_survey(self, guild_id: int, survey_id: int) -> Survey | None:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT * FROM surveys WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            ).fetchone()
        return self._survey_from_row(row) if row is not None else None

    def delete_survey(self, guild_id: int, survey_id: int) -> bool:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT status FROM surveys WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            ).fetchone()
            if row is None:
                return False
            if row["status"] != SurveyStatus.DRAFT.value:
                raise SurveyStateError("Only draft surveys can be deleted")
            # Foreign keys are not assumed to be enabled on every runtime
            # connection, so deletion is explicit and child-first.
            cursor.execute(
                "DELETE FROM survey_answers WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            )
            cursor.execute(
                "DELETE FROM survey_recipients WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            )
            cursor.execute(
                "DELETE FROM survey_questions WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            )
            cursor.execute(
                "DELETE FROM surveys WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            )
            return cursor.rowcount == 1

    def get_questions(self, guild_id: int, survey_id: int) -> list[SurveyQuestion]:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            self._require_survey_row(cursor, normalized, survey_id)
            rows = self._get_questions_with_cursor(cursor, normalized, survey_id)
        return [self._question_from_row(row) for row in rows]

    def add_question(
        self,
        guild_id: int,
        survey_id: int,
        prompt: str,
        question_type: str,
        required: bool,
    ) -> SurveyQuestion:
        normalized = self.normalize_guild_id(guild_id)
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._require_draft_row(cursor, normalized, survey_id)
            count = int(
                cursor.execute(
                    """
                    SELECT COUNT(*) FROM survey_questions
                    WHERE guild_id = ? AND survey_id = ?
                    """,
                    (normalized, survey_id),
                ).fetchone()[0]
            )
            if count >= MAX_SURVEY_QUESTIONS:
                raise SurveyStateError(
                    f"A survey can contain at most {MAX_SURVEY_QUESTIONS} questions"
                )
            result = cursor.execute(
                """
                INSERT INTO survey_questions (
                    survey_id, guild_id, position, prompt, question_type, required, created_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    survey_id,
                    normalized,
                    count + 1,
                    prompt,
                    question_type,
                    int(required),
                    now,
                ),
            )
            row = cursor.execute(
                "SELECT * FROM survey_questions WHERE question_id = ?",
                (int(result.lastrowid),),
            ).fetchone()
        return self._question_from_row(row)

    def edit_question(
        self,
        guild_id: int,
        survey_id: int,
        question_id: int,
        *,
        prompt: str | None = None,
        question_type: str | None = None,
        required: bool | None = None,
    ) -> SurveyQuestion:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._require_draft_row(cursor, normalized, survey_id)
            row = cursor.execute(
                """
                SELECT * FROM survey_questions
                WHERE guild_id = ? AND survey_id = ? AND question_id = ?
                """,
                (normalized, survey_id, question_id),
            ).fetchone()
            if row is None:
                raise SurveyNotFoundError("Survey question not found")
            cursor.execute(
                """
                UPDATE survey_questions
                SET prompt = ?, question_type = ?, required = ?
                WHERE guild_id = ? AND survey_id = ? AND question_id = ?
                """,
                (
                    row["prompt"] if prompt is None else prompt,
                    row["question_type"] if question_type is None else question_type,
                    row["required"] if required is None else int(required),
                    normalized,
                    survey_id,
                    question_id,
                ),
            )
            updated = cursor.execute(
                "SELECT * FROM survey_questions WHERE question_id = ?",
                (question_id,),
            ).fetchone()
        return self._question_from_row(updated)

    @staticmethod
    def _replace_question_order(cursor, rows: Sequence, ordered_ids: Sequence[int]) -> None:
        """Reinsert draft questions to avoid immediate UNIQUE-position conflicts."""
        by_id = {int(row["question_id"]): row for row in rows}
        if set(by_id) != set(ordered_ids):
            raise SurveyNotFoundError("Survey question not found")
        survey_id = int(rows[0]["survey_id"]) if rows else None
        guild_id = int(rows[0]["guild_id"]) if rows else None
        if survey_id is not None:
            cursor.execute(
                "DELETE FROM survey_questions WHERE guild_id = ? AND survey_id = ?",
                (guild_id, survey_id),
            )
        for position, question_id in enumerate(ordered_ids, start=1):
            row = by_id[question_id]
            cursor.execute(
                """
                INSERT INTO survey_questions (
                    question_id, survey_id, guild_id, position, prompt,
                    question_type, required, created_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    question_id,
                    row["survey_id"],
                    row["guild_id"],
                    position,
                    row["prompt"],
                    row["question_type"],
                    row["required"],
                    row["created_at"],
                ),
            )

    def remove_question(self, guild_id: int, survey_id: int, question_id: int) -> bool:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._require_draft_row(cursor, normalized, survey_id)
            rows = self._get_questions_with_cursor(cursor, normalized, survey_id)
            if question_id not in {int(row["question_id"]) for row in rows}:
                return False
            remaining = [row for row in rows if int(row["question_id"]) != question_id]
            cursor.execute(
                """
                DELETE FROM survey_questions
                WHERE guild_id = ? AND survey_id = ?
                """,
                (normalized, survey_id),
            )
            for position, row in enumerate(remaining, start=1):
                cursor.execute(
                    """
                    INSERT INTO survey_questions (
                        question_id, survey_id, guild_id, position, prompt,
                        question_type, required, created_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        row["question_id"],
                        survey_id,
                        normalized,
                        position,
                        row["prompt"],
                        row["question_type"],
                        row["required"],
                        row["created_at"],
                    ),
                )
            return True

    def move_question(
        self,
        guild_id: int,
        survey_id: int,
        question_id: int,
        new_position: int,
    ) -> list[SurveyQuestion]:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._require_draft_row(cursor, normalized, survey_id)
            rows = self._get_questions_with_cursor(cursor, normalized, survey_id)
            ordered_ids = [int(row["question_id"]) for row in rows]
            if question_id not in ordered_ids:
                raise SurveyNotFoundError("Survey question not found")
            if not 1 <= new_position <= len(ordered_ids):
                raise ValueError(f"Question position must be between 1 and {len(ordered_ids)}")
            ordered_ids.remove(question_id)
            ordered_ids.insert(new_position - 1, question_id)
            self._replace_question_order(cursor, rows, ordered_ids)
            updated = self._get_questions_with_cursor(cursor, normalized, survey_id)
        return [self._question_from_row(row) for row in updated]

    def open_survey(
        self,
        guild_id: int,
        survey_id: int,
        recipient_ids: list[int],
        *,
        target_type: str,
        registered_only: bool,
    ) -> Survey:
        normalized = self.normalize_guild_id(guild_id)
        try:
            normalized_target = SurveyTargetType(target_type)
        except ValueError as exc:
            raise ValueError("Invalid survey target type") from exc
        if normalized_target is SurveyTargetType.ALL_REGISTERED and registered_only:
            raise ValueError("all_registered targeting is already limited to registered users")
        recipients = list(dict.fromkeys(recipient_ids))
        if normalized_target is SurveyTargetType.MEMBER and len(recipients) != 1:
            raise ValueError("member targeting requires exactly one recipient")
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._require_draft_row(cursor, normalized, survey_id)
            questions = self._get_questions_with_cursor(cursor, normalized, survey_id)
            if not questions:
                raise SurveyStateError("Add at least one question before sending a survey")
            if not recipients:
                raise SurveyStateError("The survey target has no eligible recipients")
            if any(isinstance(recipient_id, bool) or recipient_id <= 0 for recipient_id in recipients):
                raise ValueError("Survey recipients must be real Discord users")

            cursor.execute(
                """
                UPDATE surveys
                SET status = 'open', target_type = ?, registered_only = ?,
                    opened_at = ?, closed_at = NULL
                WHERE guild_id = ? AND survey_id = ? AND status = 'draft'
                """,
                (
                    normalized_target.value,
                    int(registered_only),
                    now,
                    normalized,
                    survey_id,
                ),
            )
            if cursor.rowcount != 1:
                raise SurveyStateError("Survey has already been sent")
            first_question_id = int(questions[0]["question_id"])
            cursor.executemany(
                """
                INSERT INTO survey_recipients (
                    survey_id, guild_id, discord_id, delivery_status,
                    current_question_id, created_at, updated_at
                ) VALUES (?, ?, ?, 'pending', ?, ?, ?)
                """,
                [
                    (
                        survey_id,
                        normalized,
                        recipient_id,
                        first_question_id,
                        now,
                        now,
                    )
                    for recipient_id in recipients
                ],
            )
            row = cursor.execute(
                "SELECT * FROM surveys WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            ).fetchone()
        return self._survey_from_row(row)

    def close_survey(self, guild_id: int, survey_id: int) -> Survey:
        normalized = self.normalize_guild_id(guild_id)
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = self._require_survey_row(cursor, normalized, survey_id)
            if row["status"] == SurveyStatus.DRAFT.value:
                raise SurveyStateError("A draft survey cannot be closed")
            if row["status"] == SurveyStatus.CLOSED.value:
                return self._survey_from_row(row)
            cursor.execute(
                """
                UPDATE surveys SET status = 'closed', closed_at = ?
                WHERE guild_id = ? AND survey_id = ? AND status = 'open'
                """,
                (now, normalized, survey_id),
            )
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'failed',
                    claimed_at = NULL,
                    retry_requested = 0,
                    last_error = 'Survey closed before delivery', updated_at = ?
                WHERE guild_id = ? AND survey_id = ?
                  AND (
                      delivery_status = 'pending'
                      OR (
                          delivery_status = 'sending'
                          AND receipt_checked_attempt >= attempt_count
                      )
                  )
                """,
                (now, normalized, survey_id),
            )
            cursor.execute(
                """
                UPDATE survey_recipients
                SET retry_requested = 0, updated_at = ?
                WHERE guild_id = ? AND survey_id = ? AND retry_requested = 1
                """,
                (now, normalized, survey_id),
            )
            updated = cursor.execute(
                "SELECT * FROM surveys WHERE guild_id = ? AND survey_id = ?",
                (normalized, survey_id),
            ).fetchone()
        return self._survey_from_row(updated)

    def _recover_stale_with_cursor(self, cursor, stale_after_seconds: int, now: int) -> int:
        cutoff = now - stale_after_seconds
        cursor.execute(
            """
            UPDATE survey_recipients
            SET delivery_status = 'pending', claimed_at = NULL, updated_at = ?
            WHERE delivery_status = 'sending' AND claimed_at <= ?
              AND receipt_checked_attempt >= attempt_count
              AND EXISTS (
                  SELECT 1 FROM surveys
                  WHERE surveys.survey_id = survey_recipients.survey_id
                    AND surveys.guild_id = survey_recipients.guild_id
                    AND surveys.status = 'open'
              )
            """,
            (now, cutoff),
        )
        return cursor.rowcount

    def recover_stale_deliveries(self, stale_after_seconds: int) -> int:
        if stale_after_seconds < 0:
            raise ValueError("stale_after_seconds cannot be negative")
        now = self._now()
        with self.atomic_transaction() as conn:
            return self._recover_stale_with_cursor(
                conn.cursor(), stale_after_seconds, now
            )

    def list_unconfirmed_deliveries(
        self,
        guild_id: int | None = None,
        survey_id: int | None = None,
        sending_stale_after_seconds: int = 300,
        failed_grace_seconds: int = 30,
    ) -> list[SurveyRecipient]:
        """Return attempts that must be checked against DM history before retry."""
        if (guild_id is None) != (survey_id is None):
            raise ValueError("guild_id and survey_id must be provided together")
        if sending_stale_after_seconds < 0 or failed_grace_seconds < 0:
            raise ValueError("delivery receipt ages cannot be negative")
        now = self._now()
        sql = """
            SELECT recipients.*
            FROM survey_recipients AS recipients
            JOIN surveys
              ON surveys.survey_id = recipients.survey_id
             AND surveys.guild_id = recipients.guild_id
            WHERE surveys.status IN ('open', 'closed')
              AND recipients.delivery_status IN ('pending', 'sending', 'failed')
              AND recipients.dm_message_id IS NULL
              AND recipients.attempt_count > recipients.receipt_checked_attempt
              AND (
                  (
                      recipients.delivery_status = 'sending'
                      AND COALESCE(recipients.claimed_at, recipients.updated_at) <= ?
                  )
                  OR
                  (
                      recipients.delivery_status IN ('pending', 'failed')
                      AND recipients.updated_at <= ?
                  )
              )
        """
        params: tuple[int, ...] = (
            now - sending_stale_after_seconds,
            now - failed_grace_seconds,
        )
        if guild_id is not None and survey_id is not None:
            sql += " AND recipients.guild_id = ? AND recipients.survey_id = ?"
            params += (self.normalize_guild_id(guild_id), survey_id)
        sql += " ORDER BY recipients.recipient_id"
        with self.connection() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [self._recipient_from_row(row) for row in rows]

    def claim_deliveries(
        self,
        limit: int,
        stale_after_seconds: int,
    ) -> list[SurveyRecipient]:
        if limit < 1:
            raise ValueError("Delivery claim limit must be positive")
        if stale_after_seconds < 0:
            raise ValueError("stale_after_seconds cannot be negative")
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._recover_stale_with_cursor(cursor, stale_after_seconds, now)
            rows = cursor.execute(
                """
                SELECT recipients.*
                FROM survey_recipients AS recipients
                JOIN surveys
                  ON surveys.survey_id = recipients.survey_id
                 AND surveys.guild_id = recipients.guild_id
                WHERE recipients.delivery_status = 'pending'
                  AND recipients.receipt_checked_attempt >= recipients.attempt_count
                  AND surveys.status = 'open'
                ORDER BY recipients.recipient_id
                LIMIT ?
                """,
                (limit,),
            ).fetchall()
            recipient_ids = [int(row["recipient_id"]) for row in rows]
            if not recipient_ids:
                return []
            placeholders = ",".join("?" for _ in recipient_ids)
            cursor.execute(
                f"""
                UPDATE survey_recipients
                SET delivery_status = 'sending', attempt_count = attempt_count + 1,
                    claimed_at = ?, retry_requested = 0,
                    last_error = NULL, updated_at = ?
                WHERE recipient_id IN ({placeholders}) AND delivery_status = 'pending'
                """,
                (now, now, *recipient_ids),
            )
            claimed = cursor.execute(
                f"""
                SELECT * FROM survey_recipients
                WHERE recipient_id IN ({placeholders})
                ORDER BY recipient_id
                """,
                recipient_ids,
            ).fetchall()
        return [self._recipient_from_row(row) for row in claimed]

    def renew_delivery_claim(
        self,
        recipient_id: int,
        expected_attempt_count: int,
    ) -> SurveyRecipient:
        """Renew and validate the exact lease immediately before Discord I/O."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE survey_recipients
                SET claimed_at = ?, updated_at = ?
                WHERE recipient_id = ?
                  AND delivery_status = 'sending'
                  AND attempt_count = ?
                  AND receipt_checked_attempt < attempt_count
                  AND EXISTS (
                      SELECT 1 FROM surveys
                      WHERE surveys.survey_id = survey_recipients.survey_id
                        AND surveys.guild_id = survey_recipients.guild_id
                        AND surveys.status = 'open'
                  )
                """,
                (now, now, recipient_id, expected_attempt_count),
            )
            if cursor.rowcount != 1:
                raise SurveyStateError("Survey delivery claim is no longer active")
            updated = self._get_recipient_by_id(cursor, recipient_id)
        return self._recipient_from_row(updated)

    def mark_delivery_sent(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        dm_channel_id: int,
        dm_message_id: int,
    ) -> SurveyRecipient:
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = self._get_recipient_by_id(cursor, recipient_id)
            if row is None:
                raise SurveyNotFoundError("Survey recipient not found")
            row_attempt = int(row["attempt_count"])
            if (
                row["delivery_status"] == SurveyDeliveryStatus.SENT.value
                and row_attempt == expected_attempt_count
            ):
                return self._recipient_from_row(row)
            if (
                row["delivery_status"] != SurveyDeliveryStatus.SENDING.value
                or row_attempt != expected_attempt_count
            ):
                raise SurveyStateError("Survey delivery claim is no longer active")
            survey = cursor.execute(
                """
                SELECT status FROM surveys
                WHERE survey_id = ? AND guild_id = ?
                """,
                (row["survey_id"], row["guild_id"]),
            ).fetchone()
            if survey is None or survey["status"] != SurveyStatus.OPEN.value:
                raise SurveyStateError("Cannot complete delivery for a closed survey")
            if row["delivery_status"] != SurveyDeliveryStatus.SENDING.value:
                raise SurveyStateError("Survey delivery is not currently claimed")
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'sent', claimed_at = NULL,
                    dm_channel_id = ?, dm_message_id = ?,
                    ui_channel_id = ?, ui_message_id = ?,
                    controls_finalized = 0,
                    receipt_checked_attempt = attempt_count,
                    retry_requested = 0, last_error = NULL, updated_at = ?
                WHERE recipient_id = ? AND delivery_status = 'sending'
                  AND attempt_count = ?
                """,
                (
                    dm_channel_id,
                    dm_message_id,
                    dm_channel_id,
                    dm_message_id,
                    now,
                    recipient_id,
                    expected_attempt_count,
                ),
            )
            if cursor.rowcount != 1:
                raise SurveyStateError("Survey delivery claim is no longer active")
            updated = self._get_recipient_by_id(cursor, recipient_id)
        return self._recipient_from_row(updated)

    def mark_delivery_failed(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        error: str,
    ) -> SurveyRecipient:
        now = self._now()
        clean_error = str(error).strip()[:500] or "DM delivery failed"
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = self._get_recipient_by_id(cursor, recipient_id)
            if row is None:
                raise SurveyNotFoundError("Survey recipient not found")
            row_attempt = int(row["attempt_count"])
            if (
                row["delivery_status"]
                in (
                    SurveyDeliveryStatus.SENT.value,
                    SurveyDeliveryStatus.FAILED.value,
                )
                and row_attempt == expected_attempt_count
            ):
                return self._recipient_from_row(row)
            if (
                row["delivery_status"] != SurveyDeliveryStatus.SENDING.value
                or row_attempt != expected_attempt_count
            ):
                raise SurveyStateError("Survey delivery claim is no longer active")
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'failed',
                    last_error = ?, updated_at = ?
                WHERE recipient_id = ? AND delivery_status = 'sending'
                  AND attempt_count = ?
                """,
                (clean_error, now, recipient_id, expected_attempt_count),
            )
            if cursor.rowcount != 1:
                raise SurveyStateError("Survey delivery claim is no longer active")
            updated = self._get_recipient_by_id(cursor, recipient_id)
        return self._recipient_from_row(updated)

    def reconcile_delivery_sent(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        dm_channel_id: int,
        dm_message_id: int,
    ) -> SurveyRecipient:
        """Attach a DM receipt found in history, including after campaign close."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = self._get_recipient_by_id(cursor, recipient_id)
            if row is None:
                raise SurveyNotFoundError("Survey recipient not found")
            row_attempt = int(row["attempt_count"])
            if (
                row["delivery_status"] == SurveyDeliveryStatus.SENT.value
                and row_attempt == expected_attempt_count
            ):
                return self._recipient_from_row(row)
            if (
                row["delivery_status"]
                not in (
                    SurveyDeliveryStatus.PENDING.value,
                    SurveyDeliveryStatus.SENDING.value,
                    SurveyDeliveryStatus.FAILED.value,
                )
                or row_attempt != expected_attempt_count
            ):
                raise SurveyStateError("Survey delivery claim is no longer active")
            survey = cursor.execute(
                """
                SELECT status FROM surveys
                WHERE survey_id = ? AND guild_id = ?
                """,
                (row["survey_id"], row["guild_id"]),
            ).fetchone()
            if survey is None or survey["status"] == SurveyStatus.DRAFT.value:
                raise SurveyStateError("Survey delivery campaign is unavailable")
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'sent', claimed_at = NULL,
                    dm_channel_id = ?, dm_message_id = ?,
                    ui_channel_id = ?, ui_message_id = ?,
                    controls_finalized = 0,
                    receipt_checked_attempt = attempt_count,
                    retry_requested = 0, last_error = NULL, updated_at = ?
                WHERE recipient_id = ?
                  AND delivery_status IN ('pending', 'sending', 'failed')
                  AND attempt_count = ?
                """,
                (
                    dm_channel_id,
                    dm_message_id,
                    dm_channel_id,
                    dm_message_id,
                    now,
                    recipient_id,
                    expected_attempt_count,
                ),
            )
            if cursor.rowcount != 1:
                raise SurveyStateError("Survey delivery claim is no longer active")
            updated = self._get_recipient_by_id(cursor, recipient_id)
        return self._recipient_from_row(updated)

    def mark_delivery_receipt_checked(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        expected_claimed_at: int | None,
        expected_delivery_status: str,
        expected_updated_at: int,
    ) -> bool:
        """Record that DM history has no receipt for this exact attempt."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE survey_recipients
                SET receipt_checked_attempt = ?,
                    delivery_status = CASE
                        WHEN delivery_status IN ('pending', 'sending')
                         AND EXISTS (
                            SELECT 1 FROM surveys
                            WHERE surveys.survey_id = survey_recipients.survey_id
                              AND surveys.guild_id = survey_recipients.guild_id
                              AND surveys.status = 'closed'
                         )
                        THEN 'failed'
                        ELSE delivery_status
                    END,
                    claimed_at = CASE
                        WHEN delivery_status IN ('pending', 'sending')
                         AND EXISTS (
                            SELECT 1 FROM surveys
                            WHERE surveys.survey_id = survey_recipients.survey_id
                              AND surveys.guild_id = survey_recipients.guild_id
                              AND surveys.status = 'closed'
                         )
                        THEN NULL
                        ELSE claimed_at
                    END,
                    updated_at = ?
                WHERE recipient_id = ?
                  AND delivery_status IN ('pending', 'sending', 'failed')
                  AND attempt_count = ?
                  AND receipt_checked_attempt < ?
                  AND claimed_at IS ?
                  AND delivery_status = ?
                  AND updated_at = ?
                """,
                (
                    expected_attempt_count,
                    now,
                    recipient_id,
                    expected_attempt_count,
                    expected_attempt_count,
                    expected_claimed_at,
                    expected_delivery_status,
                    expected_updated_at,
                ),
            )
            return cursor.rowcount == 1

    def retry_failed_deliveries(self, guild_id: int, survey_id: int) -> int:
        normalized = self.normalize_guild_id(guild_id)
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            survey = self._require_survey_row(cursor, normalized, survey_id)
            if survey["status"] != SurveyStatus.OPEN.value:
                raise SurveyStateError("Only an open survey can retry failed DMs")
            cursor.execute(
                """
                UPDATE survey_recipients
                SET retry_requested = 1
                WHERE guild_id = ? AND survey_id = ? AND delivery_status = 'failed'
                """,
                (normalized, survey_id),
            )
            requested = cursor.rowcount
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'pending', claimed_at = NULL,
                    retry_requested = 0, last_error = NULL, updated_at = ?
                WHERE guild_id = ? AND survey_id = ? AND delivery_status = 'failed'
                  AND retry_requested = 1
                  AND receipt_checked_attempt >= attempt_count
                """,
                (now, normalized, survey_id),
            )
            return requested

    def queue_requested_delivery_retries(self) -> int:
        """Queue retry intents whose ambiguous receipt checks have completed."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE survey_recipients
                SET delivery_status = 'pending', claimed_at = NULL,
                    retry_requested = 0, last_error = NULL, updated_at = ?
                WHERE delivery_status = 'failed'
                  AND retry_requested = 1
                  AND receipt_checked_attempt >= attempt_count
                  AND EXISTS (
                      SELECT 1 FROM surveys
                      WHERE surveys.survey_id = survey_recipients.survey_id
                        AND surveys.guild_id = survey_recipients.guild_id
                        AND surveys.status = 'open'
                  )
                """,
                (now,),
            )
            return cursor.rowcount

    def _session_with_cursor(
        self,
        cursor,
        survey_id: int,
        discord_id: int,
    ) -> SurveySession | None:
        recipient_row = cursor.execute(
            """
            SELECT recipients.*
            FROM survey_recipients AS recipients
            JOIN surveys
              ON surveys.survey_id = recipients.survey_id
             AND surveys.guild_id = recipients.guild_id
            WHERE recipients.survey_id = ? AND recipients.discord_id = ?
            """,
            (survey_id, discord_id),
        ).fetchone()
        if recipient_row is None:
            return None
        guild_id = int(recipient_row["guild_id"])
        survey_row = cursor.execute(
            "SELECT * FROM surveys WHERE guild_id = ? AND survey_id = ?",
            (guild_id, survey_id),
        ).fetchone()
        if survey_row is None:
            return None
        question_rows = self._get_questions_with_cursor(cursor, guild_id, survey_id)
        answer_rows = cursor.execute(
            """
            SELECT answers.*
            FROM survey_answers AS answers
            JOIN survey_questions AS questions
              ON questions.question_id = answers.question_id
             AND questions.survey_id = answers.survey_id
             AND questions.guild_id = answers.guild_id
            WHERE answers.guild_id = ? AND answers.survey_id = ?
              AND answers.recipient_id = ?
            ORDER BY questions.position
            """,
            (guild_id, survey_id, recipient_row["recipient_id"]),
        ).fetchall()
        return SurveySession(
            survey=self._survey_from_row(survey_row),
            recipient=self._recipient_from_row(recipient_row),
            questions=tuple(self._question_from_row(row) for row in question_rows),
            answers=tuple(self._answer_from_row(row) for row in answer_rows),
        )

    def get_response_session(self, survey_id: int, discord_id: int) -> SurveySession | None:
        with self.connection() as conn:
            # sqlite3 does not implicitly begin transactions for SELECTs. Pin a
            # WAL snapshot so recipient progression and answers cannot come
            # from different commits while a DM interaction is being handled.
            conn.execute("BEGIN")
            return self._session_with_cursor(conn.cursor(), survey_id, discord_id)

    def list_recoverable_response_sessions(self) -> list[SurveySession]:
        """Return DM sessions whose stored message needs reconciliation.

        Deliberately includes submitted and closed sessions so restart
        recovery can replace stale controls with their durable final state.
        Callers should register interactive views only for the open,
        unsubmitted subset.
        """
        with self.connection() as conn:
            conn.execute("BEGIN")
            cursor = conn.cursor()
            keys = cursor.execute(
                """
                SELECT recipients.survey_id, recipients.discord_id
                FROM survey_recipients AS recipients
                JOIN surveys
                  ON surveys.survey_id = recipients.survey_id
                 AND surveys.guild_id = recipients.guild_id
                WHERE surveys.status IN ('open', 'closed')
                  AND recipients.delivery_status = 'sent'
                  AND recipients.controls_finalized = 0
                  AND COALESCE(recipients.ui_message_id, recipients.dm_message_id) IS NOT NULL
                ORDER BY recipients.recipient_id
                """
            ).fetchall()
            sessions = [
                self._session_with_cursor(
                    cursor,
                    int(key["survey_id"]),
                    int(key["discord_id"]),
                )
                for key in keys
            ]
        return [session for session in sessions if session is not None]

    def _require_open_recipient(self, cursor, survey_id: int, discord_id: int):
        row = cursor.execute(
            """
            SELECT recipients.*, surveys.status AS survey_status,
                   surveys.guild_id AS survey_guild_id
            FROM survey_recipients AS recipients
            JOIN surveys
              ON surveys.survey_id = recipients.survey_id
             AND surveys.guild_id = recipients.guild_id
            WHERE recipients.survey_id = ? AND recipients.discord_id = ?
            """,
            (survey_id, discord_id),
        ).fetchone()
        if row is None:
            raise SurveyResponseError("This survey was not sent to you")
        if row["survey_status"] != SurveyStatus.OPEN.value:
            raise SurveyResponseError("This survey is closed")
        if row["delivery_status"] != SurveyDeliveryStatus.SENT.value:
            raise SurveyResponseError("This survey has not been delivered")
        if row["submitted_at"] is not None:
            raise SurveyResponseError("This response has already been submitted")
        return row

    @staticmethod
    def _next_unanswered_question_id(cursor, recipient_row) -> int | None:
        row = cursor.execute(
            """
            SELECT questions.question_id
            FROM survey_questions AS questions
            WHERE questions.guild_id = ? AND questions.survey_id = ?
              AND NOT EXISTS (
                  SELECT 1 FROM survey_answers AS answers
                  WHERE answers.guild_id = questions.guild_id
                    AND answers.survey_id = questions.survey_id
                    AND answers.question_id = questions.question_id
                    AND answers.recipient_id = ?
              )
            ORDER BY questions.position
            LIMIT 1
            """,
            (
                recipient_row["guild_id"],
                recipient_row["survey_id"],
                recipient_row["recipient_id"],
            ),
        ).fetchone()
        return int(row["question_id"]) if row is not None else None

    def _advance_recipient(self, cursor, recipient_row, now: int) -> None:
        next_question_id = self._next_unanswered_question_id(cursor, recipient_row)
        in_review = bool(recipient_row["in_review"]) or next_question_id is None
        cursor.execute(
            """
            UPDATE survey_recipients
            SET current_question_id = ?, in_review = ?,
                started_at = COALESCE(started_at, ?), updated_at = ?
            WHERE recipient_id = ? AND submitted_at IS NULL
            """,
            (
                None if in_review else next_question_id,
                int(in_review),
                now,
                now,
                recipient_row["recipient_id"],
            ),
        )

    @staticmethod
    def _validate_answer_value(question, numeric_value, text_value) -> tuple[int | None, str | None]:
        """Defend the atomic write path even when called without SurveyService."""
        question_type = SurveyQuestionType(question["question_type"])
        if question_type is SurveyQuestionType.NPS:
            if (
                isinstance(numeric_value, bool)
                or not isinstance(numeric_value, int)
                or not 0 <= numeric_value <= 10
                or text_value is not None
            ):
                raise SurveyResponseError("NPS answers must be a whole number from 0 to 10")
            return numeric_value, None
        if question_type is SurveyQuestionType.RATING:
            if (
                isinstance(numeric_value, bool)
                or not isinstance(numeric_value, int)
                or not 1 <= numeric_value <= 5
                or text_value is not None
            ):
                raise SurveyResponseError("Rating answers must be a whole number from 1 to 5")
            return numeric_value, None
        if numeric_value is not None or not isinstance(text_value, str):
            raise SurveyResponseError("Text questions require a text answer")
        cleaned = text_value.strip()
        if not cleaned or len(cleaned) > 1_000:
            raise SurveyResponseError("Text answers must contain 1 to 1000 characters")
        return None, cleaned

    def save_answer(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
        *,
        numeric_value: int | None,
        text_value: str | None,
    ) -> SurveySession:
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            recipient = self._require_open_recipient(cursor, survey_id, discord_id)
            question = cursor.execute(
                """
                SELECT * FROM survey_questions
                WHERE guild_id = ? AND survey_id = ? AND question_id = ?
                """,
                (recipient["guild_id"], survey_id, question_id),
            ).fetchone()
            if question is None:
                raise SurveyResponseError("Question does not belong to this survey")
            numeric_value, text_value = self._validate_answer_value(
                question,
                numeric_value,
                text_value,
            )
            if recipient["current_question_id"] != question_id:
                existing = cursor.execute(
                    """
                    SELECT numeric_value, text_value, skipped
                    FROM survey_answers
                    WHERE guild_id = ? AND survey_id = ?
                      AND recipient_id = ? AND question_id = ?
                    """,
                    (
                        recipient["guild_id"],
                        survey_id,
                        recipient["recipient_id"],
                        question_id,
                    ),
                ).fetchone()
                if (
                    existing is not None
                    and not existing["skipped"]
                    and existing["numeric_value"] == numeric_value
                    and existing["text_value"] == text_value
                ):
                    session = self._session_with_cursor(cursor, survey_id, discord_id)
                    if session is None:
                        raise RuntimeError("Survey session disappeared after duplicate answer")
                    return session
                if recipient["in_review"] and recipient["current_question_id"] is None:
                    raise SurveyResponseError(
                        "Select a question from the review summary first"
                    )
                raise SurveyResponseError("This is not the current survey question")
            cursor.execute(
                """
                INSERT INTO survey_answers (
                    survey_id, guild_id, recipient_id, question_id,
                    numeric_value, text_value, skipped, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)
                ON CONFLICT(recipient_id, question_id) DO UPDATE SET
                    numeric_value = excluded.numeric_value,
                    text_value = excluded.text_value,
                    skipped = 0,
                    updated_at = excluded.updated_at
                """,
                (
                    survey_id,
                    recipient["guild_id"],
                    recipient["recipient_id"],
                    question_id,
                    numeric_value,
                    text_value,
                    now,
                    now,
                ),
            )
            self._advance_recipient(cursor, recipient, now)
            session = self._session_with_cursor(cursor, survey_id, discord_id)
        if session is None:
            raise RuntimeError("Survey session disappeared after saving an answer")
        return session

    def skip_question(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
    ) -> SurveySession:
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            recipient = self._require_open_recipient(cursor, survey_id, discord_id)
            question = cursor.execute(
                """
                SELECT * FROM survey_questions
                WHERE guild_id = ? AND survey_id = ? AND question_id = ?
                """,
                (recipient["guild_id"], survey_id, question_id),
            ).fetchone()
            if question is None:
                raise SurveyResponseError("Question does not belong to this survey")
            if recipient["current_question_id"] != question_id:
                existing = cursor.execute(
                    """
                    SELECT skipped FROM survey_answers
                    WHERE guild_id = ? AND survey_id = ?
                      AND recipient_id = ? AND question_id = ?
                    """,
                    (
                        recipient["guild_id"],
                        survey_id,
                        recipient["recipient_id"],
                        question_id,
                    ),
                ).fetchone()
                if existing is not None and existing["skipped"]:
                    session = self._session_with_cursor(cursor, survey_id, discord_id)
                    if session is None:
                        raise RuntimeError("Survey session disappeared after duplicate skip")
                    return session
                if recipient["in_review"] and recipient["current_question_id"] is None:
                    raise SurveyResponseError(
                        "Select a question from the review summary first"
                    )
                raise SurveyResponseError("This is not the current survey question")
            if question["required"]:
                raise SurveyResponseError("This question is required")
            cursor.execute(
                """
                INSERT INTO survey_answers (
                    survey_id, guild_id, recipient_id, question_id,
                    numeric_value, text_value, skipped, created_at, updated_at
                ) VALUES (?, ?, ?, ?, NULL, NULL, 1, ?, ?)
                ON CONFLICT(recipient_id, question_id) DO UPDATE SET
                    numeric_value = NULL,
                    text_value = NULL,
                    skipped = 1,
                    updated_at = excluded.updated_at
                """,
                (
                    survey_id,
                    recipient["guild_id"],
                    recipient["recipient_id"],
                    question_id,
                    now,
                    now,
                ),
            )
            self._advance_recipient(cursor, recipient, now)
            session = self._session_with_cursor(cursor, survey_id, discord_id)
        if session is None:
            raise RuntimeError("Survey session disappeared after skipping a question")
        return session

    @staticmethod
    def _assert_response_complete(cursor, recipient_row) -> None:
        counts = cursor.execute(
            """
            SELECT
                COUNT(*) AS question_count,
                SUM(CASE WHEN answers.answer_id IS NOT NULL THEN 1 ELSE 0 END) AS answer_count,
                SUM(CASE WHEN questions.required = 1 AND (
                    answers.answer_id IS NULL OR answers.skipped = 1
                ) THEN 1 ELSE 0 END) AS missing_required
            FROM survey_questions AS questions
            LEFT JOIN survey_answers AS answers
              ON answers.guild_id = questions.guild_id
             AND answers.survey_id = questions.survey_id
             AND answers.question_id = questions.question_id
             AND answers.recipient_id = ?
            WHERE questions.guild_id = ? AND questions.survey_id = ?
            """,
            (
                recipient_row["recipient_id"],
                recipient_row["guild_id"],
                recipient_row["survey_id"],
            ),
        ).fetchone()
        question_count = int(counts["question_count"] or 0)
        answer_count = int(counts["answer_count"] or 0)
        missing_required = int(counts["missing_required"] or 0)
        if question_count == 0 or answer_count != question_count or missing_required:
            raise SurveyResponseError("Answer or skip every question before reviewing")

    def begin_review(self, survey_id: int, discord_id: int) -> SurveySession:
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            recipient = self._require_open_recipient(cursor, survey_id, discord_id)
            self._assert_response_complete(cursor, recipient)
            cursor.execute(
                """
                UPDATE survey_recipients
                SET current_question_id = NULL, in_review = 1, updated_at = ?
                WHERE recipient_id = ? AND submitted_at IS NULL
                """,
                (now, recipient["recipient_id"]),
            )
            session = self._session_with_cursor(cursor, survey_id, discord_id)
        if session is None:
            raise RuntimeError("Survey session disappeared while entering review")
        return session

    def select_response_question(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
    ) -> SurveySession:
        """Persist which question an in-review respondent is currently editing."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            recipient = self._require_open_recipient(cursor, survey_id, discord_id)
            if not recipient["in_review"]:
                raise SurveyResponseError("Finish the survey before editing review answers")
            question = cursor.execute(
                """
                SELECT question_id FROM survey_questions
                WHERE guild_id = ? AND survey_id = ? AND question_id = ?
                """,
                (recipient["guild_id"], survey_id, question_id),
            ).fetchone()
            if question is None:
                raise SurveyResponseError("Question does not belong to this survey")
            cursor.execute(
                """
                UPDATE survey_recipients
                SET current_question_id = ?, updated_at = ?
                WHERE recipient_id = ? AND submitted_at IS NULL AND in_review = 1
                """,
                (question_id, now, recipient["recipient_id"]),
            )
            session = self._session_with_cursor(cursor, survey_id, discord_id)
        if session is None:
            raise RuntimeError("Survey session disappeared while selecting a review answer")
        return session

    def submit_response(self, survey_id: int, discord_id: int) -> bool:
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            recipient = cursor.execute(
                """
                SELECT recipients.*, surveys.status AS survey_status
                FROM survey_recipients AS recipients
                JOIN surveys
                  ON surveys.survey_id = recipients.survey_id
                 AND surveys.guild_id = recipients.guild_id
                WHERE recipients.survey_id = ? AND recipients.discord_id = ?
                """,
                (survey_id, discord_id),
            ).fetchone()
            if recipient is None:
                raise SurveyResponseError("This survey was not sent to you")
            if recipient["submitted_at"] is not None:
                return False
            if recipient["survey_status"] != SurveyStatus.OPEN.value:
                raise SurveyResponseError("This survey is closed")
            if recipient["delivery_status"] != SurveyDeliveryStatus.SENT.value:
                raise SurveyResponseError("This survey has not been delivered")
            if not recipient["in_review"]:
                raise SurveyResponseError("Review your answers before submitting")
            if recipient["current_question_id"] is not None:
                raise SurveyResponseError("Return to the review summary before submitting")
            self._assert_response_complete(cursor, recipient)
            cursor.execute(
                """
                UPDATE survey_recipients
                SET submitted_at = ?, current_question_id = NULL,
                    in_review = 1, updated_at = ?
                WHERE recipient_id = ? AND submitted_at IS NULL
                """,
                (now, now, recipient["recipient_id"]),
            )
            return cursor.rowcount == 1

    def finalize_response_ui(
        self,
        survey_id: int,
        discord_id: int,
        expected_message_id: int,
    ) -> bool:
        """Stop restart reconciliation after final controls were removed."""
        now = self._now()
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE survey_recipients
                SET controls_finalized = 1, updated_at = ?
                WHERE survey_id = ? AND discord_id = ?
                  AND controls_finalized = 0
                  AND COALESCE(ui_message_id, dm_message_id) = ?
                  AND (
                      submitted_at IS NOT NULL
                      OR EXISTS (
                          SELECT 1 FROM surveys
                          WHERE surveys.survey_id = survey_recipients.survey_id
                            AND surveys.guild_id = survey_recipients.guild_id
                            AND surveys.status = 'closed'
                      )
                  )
                """,
                (now, survey_id, discord_id, expected_message_id),
            )
            return cursor.rowcount == 1

    @staticmethod
    def _rate(numerator: int, denominator: int) -> float:
        return numerator / denominator if denominator else 0.0

    @staticmethod
    def _round_ratio_to_nearest(numerator: int, denominator: int) -> int:
        """Round an exact ratio to the nearest integer, with half values away from zero."""
        if denominator <= 0:
            raise ValueError("denominator must be positive")
        quotient, remainder = divmod(abs(numerator), denominator)
        if remainder * 2 >= denominator:
            quotient += 1
        return quotient if numerator >= 0 else -quotient

    def get_results(self, guild_id: int, survey_id: int) -> SurveyResults:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            # Keep campaign metrics and every per-question aggregate on one
            # snapshot while submitted responses continue arriving.
            conn.execute("BEGIN")
            cursor = conn.cursor()
            survey_row = self._require_survey_row(cursor, normalized, survey_id)
            metric_row = cursor.execute(
                """
                SELECT
                    COUNT(*) AS recipient_count,
                    SUM(CASE WHEN delivery_status = 'sent' THEN 1 ELSE 0 END)
                        AS delivered_count,
                    SUM(CASE WHEN started_at IS NOT NULL THEN 1 ELSE 0 END)
                        AS started_count,
                    SUM(CASE WHEN submitted_at IS NOT NULL THEN 1 ELSE 0 END)
                        AS completed_count
                FROM survey_recipients
                WHERE guild_id = ? AND survey_id = ?
                """,
                (normalized, survey_id),
            ).fetchone()
            question_rows = self._get_questions_with_cursor(cursor, normalized, survey_id)
            question_results: list[SurveyQuestionResult] = []
            for question_row in question_rows:
                answer_rows = cursor.execute(
                    """
                    SELECT answers.numeric_value, answers.text_value, answers.skipped
                    FROM survey_answers AS answers
                    JOIN survey_recipients AS recipients
                      ON recipients.recipient_id = answers.recipient_id
                     AND recipients.survey_id = answers.survey_id
                     AND recipients.guild_id = answers.guild_id
                    WHERE answers.guild_id = ? AND answers.survey_id = ?
                      AND answers.question_id = ?
                      AND recipients.submitted_at IS NOT NULL
                    ORDER BY answers.answer_id
                    """,
                    (normalized, survey_id, question_row["question_id"]),
                ).fetchall()
                skipped_count = sum(bool(row["skipped"]) for row in answer_rows)
                answered = [row for row in answer_rows if not row["skipped"]]
                question_type = SurveyQuestionType(question_row["question_type"])
                nps_score: int | None = None
                promoters = passives = detractors = 0
                rating_average: float | None = None
                rating_distribution = [0, 0, 0, 0, 0]
                text_responses: tuple[str, ...] = ()

                if question_type is SurveyQuestionType.NPS:
                    values = [
                        int(row["numeric_value"])
                        for row in answered
                        if row["numeric_value"] is not None
                        and 0 <= int(row["numeric_value"]) <= 10
                    ]
                    promoters = sum(value >= 9 for value in values)
                    passives = sum(7 <= value <= 8 for value in values)
                    detractors = sum(value <= 6 for value in values)
                    if values:
                        nps_score = self._round_ratio_to_nearest(
                            100 * (promoters - detractors),
                            len(values),
                        )
                elif question_type is SurveyQuestionType.RATING:
                    values = [
                        int(row["numeric_value"])
                        for row in answered
                        if row["numeric_value"] is not None
                        and 1 <= int(row["numeric_value"]) <= 5
                    ]
                    if values:
                        rating_average = sum(values) / len(values)
                    for value in values:
                        rating_distribution[value - 1] += 1
                else:
                    # Content ordering, rather than respondent/answer ordering,
                    # prevents separate text-question lists from becoming an
                    # implicit cross-question correlation channel.
                    text_responses = tuple(
                        sorted(
                            str(row["text_value"])
                            for row in answered
                            if row["text_value"] is not None
                        )
                    )

                question_results.append(
                    SurveyQuestionResult(
                        question_id=int(question_row["question_id"]),
                        position=int(question_row["position"]),
                        prompt=str(question_row["prompt"]),
                        question_type=question_type,
                        response_count=len(answered),
                        skipped_count=skipped_count,
                        nps_score=nps_score,
                        promoters=promoters,
                        passives=passives,
                        detractors=detractors,
                        rating_average=rating_average,
                        rating_distribution=tuple(rating_distribution),
                        text_responses=text_responses,
                    )
                )

        recipient_count = int(metric_row["recipient_count"] or 0)
        delivered_count = int(metric_row["delivered_count"] or 0)
        started_count = int(metric_row["started_count"] or 0)
        completed_count = int(metric_row["completed_count"] or 0)
        return SurveyResults(
            survey_id=int(survey_row["survey_id"]),
            title=str(survey_row["title"]),
            status=SurveyStatus(survey_row["status"]),
            recipient_count=recipient_count,
            delivered_count=delivered_count,
            started_count=started_count,
            completed_count=completed_count,
            delivery_rate=self._rate(delivered_count, recipient_count),
            started_rate=self._rate(started_count, delivered_count),
            completion_rate=self._rate(completed_count, started_count),
            response_rate=self._rate(completed_count, delivered_count),
            questions=tuple(question_results),
        )
