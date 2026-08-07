import React, { SVGProps, memo } from 'react';

// Cardano's ada mark: a central node ringed by orbiting nodes, in Cardano blue.
function _CardanoLogo(props: SVGProps<SVGSVGElement>) {
  const ring = (radius: number, dotRadius: number, offset: number) =>
    Array.from({ length: 6 }, (_, i) => {
      const angle = ((offset + i * 60) * Math.PI) / 180;
      return (
        <circle
          key={`${radius}-${i}`}
          cx={32 + radius * Math.cos(angle)}
          cy={32 + radius * Math.sin(angle)}
          r={dotRadius}
        />
      );
    });

  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" {...props}>
      <g fill="#0033ad">
        <circle cx="32" cy="32" r="5" />
        {ring(13, 3.4, 0)}
        {ring(23, 2.6, 30)}
        {ring(29, 1.8, 0)}
      </g>
    </svg>
  );
}

export const CardanoLogo = memo(_CardanoLogo);
