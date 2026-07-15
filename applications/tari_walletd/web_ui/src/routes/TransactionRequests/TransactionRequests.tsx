//  Copyright 2026 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import {
  useApproveTransactionRequest,
  useListTransactionRequests,
  useRejectTransactionRequest,
  useSubmitTransactionRequest,
} from "@api/hooks/useTransactionRequests";
import FetchStatusCheck from "@components/FetchStatusCheck";
import PageHeading from "@components/PageHeading";
import { StyledPaper } from "@components/StyledComponents";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import Accordion from "@mui/material/Accordion";
import AccordionDetails from "@mui/material/AccordionDetails";
import AccordionSummary from "@mui/material/AccordionSummary";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Chip from "@mui/material/Chip";
import Divider from "@mui/material/Divider";
import Grid from "@mui/material/Grid";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import type { EffectiveStatus, TransactionRequestInfo } from "@tari-project/ootle-ts-bindings";
import { useEffect, useState } from "react";

function statusColor(status: EffectiveStatus): "default" | "warning" | "success" | "error" | "info" {
  switch (status) {
    case "Pending":
      return "warning";
    case "Approved":
      return "info";
    case "Submitted":
      return "success";
    case "Rejected":
      return "error";
    default:
      return "default";
  }
}

function Countdown({ expiresAt }: { expiresAt: bigint }) {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, []);

  const remaining = Math.max(0, Math.floor(Number(expiresAt) - now));
  const mins = Math.floor(remaining / 60);
  const secs = remaining % 60;
  return <Chip label={`expires in ${mins}:${secs.toString().padStart(2, "0")}`} size="small" variant="outlined" />;
}

function ValueSummary({ request }: { request: TransactionRequestInfo }) {
  // A stealth transfer's instructions are commitments and range proofs, so a
  // person cannot read an amount out of them. The wallet owns the masks and is
  // the only party that can say what leaves. Without this the approver is
  // authorising an opaque blob.
  if (!request.value_summary) {
    return null;
  }
  const { amount_leaving, inputs_total, change_total, resource_address } = request.value_summary;
  return (
    <Alert severity="warning" icon={false} sx={{ mb: 2 }}>
      <Typography variant="h6" sx={{ fontWeight: 600 }}>
        {Number(amount_leaving).toLocaleString()} µT leaves this wallet
      </Typography>
      <Typography variant="body2" sx={{ opacity: 0.85 }}>
        {Number(inputs_total).toLocaleString()} µT spent, {Number(change_total).toLocaleString()} µT returned as change
      </Typography>
      <Typography variant="body2" sx={{ opacity: 0.7, wordBreak: "break-all" }}>
        {resource_address}
      </Typography>
    </Alert>
  );
}

function RequestCard({ request }: { request: TransactionRequestInfo }) {
  const approve = useApproveTransactionRequest();
  const reject = useRejectTransactionRequest();
  const submit = useSubmitTransactionRequest();
  const isActionable = request.status === "Pending";
  // Approving does not broadcast: submit is a separate permission, so an
  // approved request waits here until someone holding it acts.
  const isSubmittable = request.status === "Approved";
  const busy = approve.isPending || reject.isPending || submit.isPending;
  const error = approve.error ?? reject.error ?? submit.error;

  // Pin the decision to the bytes rendered here. If the request changed
  // underneath this view, the daemon refuses rather than authorising something
  // the approver never saw.
  const params = { request_id: request.request_id, transaction_hash: request.transaction_hash };

  const v1 = request.transaction.V1;
  const instructions = v1?.instructions ?? [];
  const feeInstructions = v1?.fee_instructions ?? [];
  const isSealSignerAuthorized = v1?.is_seal_signer_authorized ?? false;

  return (
    <StyledPaper sx={{ mb: 2 }}>
      <Stack direction="row" justifyContent="space-between" alignItems="center" sx={{ mb: 1 }} flexWrap="wrap" gap={1}>
        <Typography variant="h5">
          {request.requested_by ? `"${request.requested_by}"` : "A wallet session"} requests approval
        </Typography>
        <Stack direction="row" spacing={1} alignItems="center">
          {isActionable && <Countdown expiresAt={request.expires_at} />}
          <Chip label={request.status} size="small" color={statusColor(request.status)} />
        </Stack>
      </Stack>

      <ValueSummary request={request} />

      <Grid container spacing={2} sx={{ mb: 1 }}>
        <Grid size={{ xs: 12, md: 6 }}>
          <Typography variant="body2" sx={{ opacity: 0.7 }}>
            Seal signer
          </Typography>
          <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap">
            <Typography variant="body2" sx={{ fontFamily: "monospace", wordBreak: "break-all" }}>
              {JSON.stringify(request.seal_signer)}
            </Typography>
            {/* Not an implementation detail: when set, the seal signature also
                authorises the transaction, lending the sealer's account
                authority to these instructions. */}
            <Chip
              label={isSealSignerAuthorized ? "authorized" : "not authorized"}
              size="small"
              color={isSealSignerAuthorized ? "warning" : "default"}
              variant="outlined"
            />
          </Stack>
        </Grid>
        <Grid size={{ xs: 12, md: 6 }}>
          <Typography variant="body2" sx={{ opacity: 0.7 }}>
            Approving commits to
          </Typography>
          <Typography variant="body2" sx={{ fontFamily: "monospace", wordBreak: "break-all" }}>
            {request.transaction_hash}
          </Typography>
        </Grid>
      </Grid>

      <Accordion elevation={0} disableGutters>
        <AccordionSummary expandIcon={<ExpandMoreIcon />}>
          <Typography variant="body2">
            {instructions.length} instruction{instructions.length === 1 ? "" : "s"}
            {feeInstructions.length > 0 &&
              `, ${feeInstructions.length} fee instruction${feeInstructions.length === 1 ? "" : "s"}`}
          </Typography>
        </AccordionSummary>
        <AccordionDetails>
          <Box
            component="pre"
            sx={{ overflowX: "auto", fontSize: "0.75rem", m: 0, p: 1, borderRadius: 1, bgcolor: "action.hover" }}
          >
            {JSON.stringify({ fee_instructions: feeInstructions, instructions }, null, 2)}
          </Box>
        </AccordionDetails>
      </Accordion>

      {error && (
        <Alert severity="error" sx={{ mt: 1 }}>
          {error.message}
        </Alert>
      )}

      {(isActionable || isSubmittable) && (
        <>
          <Divider sx={{ my: 2 }} />
          <Stack direction="row" spacing={1} justifyContent="flex-end">
            {isActionable && (
              <>
                <Button variant="outlined" color="error" disabled={busy} onClick={() => reject.mutate(params)}>
                  Reject
                </Button>
                <Button variant="contained" disabled={busy} onClick={() => approve.mutate(params)}>
                  Approve
                </Button>
              </>
            )}
            {isSubmittable && (
              <Button
                variant="contained"
                disabled={busy}
                onClick={() => submit.mutate({ request_id: request.request_id })}
              >
                Submit
              </Button>
            )}
          </Stack>
        </>
      )}
    </StyledPaper>
  );
}

export default function TransactionRequests() {
  const { data, isFetching, isError, error } = useListTransactionRequests();

  const requests = data?.requests ?? [];
  const pending = requests.filter((r) => r.status === "Pending");
  const rest = requests.filter((r) => r.status !== "Pending");

  return (
    <Grid container spacing={5}>
      <Grid size={12}>
        <PageHeading>Transaction Requests</PageHeading>
      </Grid>
      <Grid size={12}>
        <FetchStatusCheck isLoading={isFetching && !data} isError={isError} errorMessage={error?.message ?? ""}>
          {requests.length === 0 && (
            <StyledPaper>
              <Typography variant="body1" sx={{ opacity: 0.7 }}>
                Nothing waiting. A tool holding <code>transaction_requests:create</code> can ask for approval here.
              </Typography>
            </StyledPaper>
          )}
          {pending.map((r) => (
            <RequestCard key={r.request_id} request={r} />
          ))}
          {rest.length > 0 && (
            <>
              <Typography variant="h5" sx={{ mt: 4, mb: 2 }}>
                History
              </Typography>
              {rest.map((r) => (
                <RequestCard key={r.request_id} request={r} />
              ))}
            </>
          )}
        </FetchStatusCheck>
      </Grid>
    </Grid>
  );
}
