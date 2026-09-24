//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause
import { Stack, Table, TableBody, TableCell, TableContainer, TableHead, TableRow, Typography } from "@mui/material";
import type { DownSubstate, UpSubstate } from "@tari-project/ootle-ts-bindings";
import { substateIdToString } from "@tari-project/ootle-ts-bindings";
import CopyToClipboard from "../../../Components/CopyToClipboard";
import { DataTableCell } from "../../../Components/StyledComponents";

export default function SubstateChanges({ upped, downed }: { upped: UpSubstate[]; downed: DownSubstate[] }) {
  const changes = [
    ...upped.map((up) => ({ change: "Up", substate_id: up.substate_id, version: up.version })),
    ...downed.map((down) => ({ change: "Down", substate_id: down.substate_id, version: down.version })),
  ];
  return (
    <TableContainer>
      <Table>
        <TableHead>
          <TableRow>
            <TableCell>Change</TableCell>
            <TableCell>Substate ID</TableCell>
            <TableCell>Version</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {changes.map((item, index) => {
            const idStr = substateIdToString(item.substate_id);
            return (
              <TableRow key={index}>
                <DataTableCell>{item.change}</DataTableCell>
                <DataTableCell>
                  <Stack direction="row" alignItems="center">
                    <Typography variant="body2" sx={{ fontFamily: "monospace", wordBreak: "break-all" }}>
                      {idStr}
                    </Typography>
                    <CopyToClipboard copy={idStr} />
                  </Stack>
                </DataTableCell>
                <DataTableCell>{item.version}</DataTableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </TableContainer>
  );
}
